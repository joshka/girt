use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::fd::OwnedFd;
use rustix::fs::{Dir, FileType, Stat, fstat, openat};

use super::{PreparedWorktree, WorktreeError, WorktreeFile, WorktreeLimits};
use crate::apply::{Patch, TreeLimits, TreeOptions};
use crate::status::worktree::{directory_flags, open_root, optional_stat, repository_marker, same};
use crate::worktree_names::{NamePolicy, Protection};
use crate::{EntryMode, Repository, filter};

type Result<T> = std::result::Result<T, WorktreeError>;

#[derive(Debug)]
pub(super) struct Leaf {
    pub(super) stat: Stat,
    pub(super) bytes: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct Snapshot {
    pub(super) root: PathBuf,
    pub(super) directories: BTreeMap<Vec<u8>, Option<Stat>>,
    pub(super) leaves: BTreeMap<Vec<u8>, Option<Leaf>>,
    pub(super) removals: BTreeSet<Vec<u8>>,
    pub(super) limits: WorktreeLimits,
    policy: NamePolicy,
    path_bytes: usize,
}

pub(super) fn prepare(
    patch: &Patch,
    repo: &Repository,
    filters: &mut filter::Pipeline,
    options: TreeOptions,
    limits: WorktreeLimits,
    cancel: &AtomicBool,
) -> Result<PreparedWorktree> {
    check(cancel)?;
    if patch.files().len() > limits.content.max_files {
        return Err(WorktreeError::Limit("files"));
    }
    let root = repo
        .worktree()
        .ok_or_else(|| refused(b"", "repository has no working tree"))?;
    if !repo
        .config()
        .boolean("core", None, "filemode")
        .map_err(|_| refused(b"", "invalid core.fileMode boolean"))?
        .unwrap_or(true)
    {
        return Err(refused(
            b"",
            "core.fileMode=false requires an explicit mode policy",
        ));
    }
    let fd = open_root(root)?;
    let root_stat = fstat(&fd).map_err(|e| io(b"", e))?;
    let protection = Protection::from_config(repo).map_err(|reason| refused(b"", reason))?;
    let policy = NamePolicy::from_config(repo).map_err(|reason| refused(b"", reason))?;
    let mut snapshot = Snapshot {
        root: root.to_path_buf(),
        directories: BTreeMap::from([(Vec::new(), Some(root_stat))]),
        leaves: BTreeMap::new(),
        removals: BTreeSet::new(),
        limits,
        policy,
        path_bytes: 0,
    };
    if limits.max_directories == 0 {
        return Err(WorktreeError::Limit("directories"));
    }
    let path_limits = TreeLimits {
        max_depth: limits.max_depth,
        ..TreeLimits::default()
    };
    let (mappings, removals) = crate::apply::tree::map_files(patch, &mut |path| {
        check(cancel).map_err(|_| crate::apply::TreeError::Cancelled)?;
        let path = crate::apply::tree::mapped(
            path,
            &options,
            path_limits,
            limits.max_path_bytes.saturating_sub(snapshot.path_bytes),
        )?;
        snapshot.path_bytes += path.len();
        Ok(path)
    })?;
    snapshot.removals = removals;
    for mapping in &mappings {
        if mapping.file.operation() != crate::apply::FileOperation::Copy
            && let Some(path) = &mapping.old
        {
            snapshot.removals.insert(path.clone());
        }
    }
    let mut aliases = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for mapping in &mappings {
        for path in mapping.old.iter().chain(mapping.new.iter()) {
            if protection.refuses(path) {
                return Err(refused(path, "repository metadata name alias"));
            }
            let mut prefix = Vec::new();
            let mut folded = Vec::new();
            for name in path.split(|b| *b == b'/') {
                if name.len() > 255 {
                    return Err(WorktreeError::Limit("path component bytes"));
                }
                if !prefix.is_empty() {
                    prefix.push(b'/');
                }
                prefix.extend_from_slice(name);
                folded.push(policy.key(name));
                if let Some(previous) = aliases.get(&folded) {
                    if previous != &prefix {
                        return Err(refused(path, "planned paths share a volume name"));
                    }
                } else {
                    if aliases.len()
                        >= limits
                            .max_directories
                            .saturating_add(patch.files().len().saturating_mul(2))
                    {
                        return Err(WorktreeError::Limit("planned path prefixes"));
                    }
                    if prefix.len() > limits.max_path_bytes.saturating_sub(snapshot.path_bytes) {
                        return Err(WorktreeError::Limit("path bytes"));
                    }
                    snapshot.path_bytes += prefix.len();
                    aliases.insert(folded.clone(), prefix.clone());
                }
            }
            paths.insert(path.clone());
        }
    }
    let mut entries = limits.max_directory_entries;
    let mut retained = limits.max_retained_bytes;
    for path in paths {
        check(cancel)?;
        let leaf = match snapshot.parent(&path, true, &mut entries, cancel)? {
            None => None,
            Some(parent) => match optional_stat(&parent, name(&path), &path)? {
                None => None,
                Some(stat) => {
                    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                        return Err(refused(
                            &path,
                            "patch sources and destinations must be regular files",
                        ));
                    }
                    let bytes = crate::checkout::read_leaf(
                        &parent,
                        &path,
                        &stat,
                        limits.content.max_input_bytes.min(retained),
                        cancel,
                    )?;
                    charge(&mut retained, bytes.len(), "payload bytes")?;
                    Some(Leaf { stat, bytes })
                }
            },
        };
        snapshot.leaves.insert(path, leaf);
    }
    let index = repo.read_index(limits.index)?;
    let objects = repo.objects_controlled(
        crate::PackLimits::default(),
        crate::AlternateLimits::default(),
        cancel,
    )?;
    let mut files = Vec::new();
    for mapping in &mappings {
        check(cancel)?;
        let source = mapping
            .old
            .as_ref()
            .map(|path| {
                snapshot.leaves[path]
                    .as_ref()
                    .ok_or_else(|| refused(path, "source is absent"))
            })
            .transpose()?;
        if let Some(destination) = &mapping.new
            && snapshot.leaves[destination].is_some()
            && mapping.old.as_ref() != Some(destination)
            && !snapshot.removals.contains(destination)
        {
            return Err(refused(destination, "addition destination exists"));
        }
        let mode = source.map_or(EntryMode::Blob, |leaf| {
            if leaf.stat.st_mode & 0o100 != 0 {
                EntryMode::Executable
            } else {
                EntryMode::Blob
            }
        });
        if mapping
            .file
            .old_mode()
            .is_some_and(|expected| expected != mode || source.is_none())
        {
            return Err(refused(
                mapping.old.as_deref().unwrap_or_default(),
                "declared source mode differs",
            ));
        }
        let new_mode = mapping.file.new_mode().unwrap_or(mode);
        if !matches!(new_mode, EntryMode::Blob | EntryMode::Executable) {
            return Err(refused(
                mapping.new.as_deref().unwrap_or_default(),
                "symlink and special modes are unsupported",
            ));
        }
        let raw = source.map_or(b"".as_slice(), |leaf| leaf.bytes.as_slice());
        let mut unfiltered = false;
        let mut warning = None;
        let preimage = if let Some(path) = &mapping.old {
            let needs_staged = filters.plan(path)?.text() == filter::Text::Auto;
            let staged_entry = index.as_ref().filter(|_| needs_staged).and_then(|index| {
                let at = index
                    .entries()
                    .partition_point(|entry| entry.path.as_slice() < path.as_slice());
                index.entries().get(at).filter(|entry| {
                    entry.path == *path
                        && entry.stage == crate::index::Stage::Normal
                        && !entry.intent_to_add
                        && !entry.id.is_null()
                        && matches!(
                            entry.mode,
                            crate::index::Mode::Regular | crate::index::Mode::Executable
                        )
                })
            });
            let staged = staged_entry
                .map(|entry| {
                    let read = crate::ReadLimits {
                        max_object_bytes: limits.content.max_input_bytes.min(retained),
                        ..crate::ReadLimits::default()
                    };
                    objects.read_blob_with(entry.id, read)
                })
                .transpose()?;
            if let Some(bytes) = &staged {
                charge(&mut retained, bytes.len(), "payload bytes")?;
            }
            let converted = filters.to_repository(
                path,
                raw,
                staged
                    .as_deref()
                    .map_or(filter::Staged::Absent, filter::Staged::Blob),
            )?;
            unfiltered |= converted.skipped.is_some() || converted.unencoded;
            warning = converted.warning;
            charge(&mut retained, converted.bytes.len(), "payload bytes")?;
            if converted.bytes.len() > limits.content.max_input_bytes {
                return Err(WorktreeError::Limit("stored preimage bytes"));
            }
            converted.bytes.into_owned()
        } else {
            Vec::new()
        };
        let applied = mapping
            .file
            .apply_with(&preimage, limits.content)
            .map_err(|source| crate::apply::TreeError::Content {
                path: mapping
                    .new
                    .as_ref()
                    .or(mapping.old.as_ref())
                    .unwrap()
                    .clone(),
                source,
            })?;
        charge(&mut retained, applied.content.len(), "payload bytes")?;
        let content = if let Some(path) = &mapping.new {
            let id = crate::ObjectId::for_blob(repo.object_format(), &applied.content);
            let converted = filters.to_worktree(path, &applied.content, id)?;
            unfiltered |= converted.skipped.is_some() || converted.unencoded;
            charge(&mut retained, converted.bytes.len(), "payload bytes")?;
            if converted.bytes.len() > limits.content.max_output_bytes {
                return Err(WorktreeError::Limit("working postimage bytes"));
            }
            Some(converted.bytes.into_owned())
        } else {
            None
        };
        files.push(WorktreeFile {
            path: mapping
                .new
                .as_ref()
                .or(mapping.old.as_ref())
                .unwrap()
                .clone(),
            source_path: mapping.old.clone(),
            hunks: applied.hunks,
            unfiltered,
            crlf_warning: warning,
            content,
            executable: new_mode == EntryMode::Executable,
        });
    }
    snapshot.check_projection(&files, &mut entries, cancel)?;
    Ok(PreparedWorktree { files, snapshot })
}

impl Snapshot {
    /// Reserve a conservative upper bound for every planned directory scan before mutation.
    /// Replacements and newly created directories can only increase these original name counts
    /// by the number of planned creations. Unrelated concurrent writes are excluded by contract;
    /// the live budget and name guards still refuse observed changes during publication.
    fn admit_publication(&mut self, files: &[WorktreeFile], cancel: &AtomicBool) -> Result<()> {
        let directories: Vec<_> = self.directories.keys().cloned().collect();
        let mut inspection = self.limits.max_directory_entries;
        let mut counts = BTreeMap::new();
        for path in directories {
            check(cancel)?;
            let probe = join(&path, b"");
            let count = match self.parent(&probe, false, &mut inspection, cancel)? {
                Some(fd) => self.names(&fd, &path, b"", &mut inspection, cancel)?.len(),
                None => 0,
            };
            counts.insert(path, count);
        }
        let destinations: BTreeSet<_> = files
            .iter()
            .filter(|file| file.content.is_some())
            .map(|file| file.path.as_slice())
            .collect();
        let mut weights: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
        for path in &self.removals {
            weight(&mut weights, path, 2)?;
        }
        for file in files {
            if file.content.is_some() {
                // Destination absence, open for create, pre-write reachability and final check.
                weight(&mut weights, &file.path, 4)?;
                increment(&mut counts, parent(&file.path))?;
                if let Some(source) = &file.source_path
                    && !self.removals.contains(source)
                {
                    weight(&mut weights, source, 1)?;
                }
            }
        }
        for (path, stat) in &self.directories {
            if stat.is_none() {
                weight(&mut weights, path, 1)?;
                increment(&mut counts, parent(path))?;
            }
        }
        for path in self.leaves.keys() {
            if !destinations.contains(path.as_slice()) {
                weight(&mut weights, path, 1)?;
            }
        }
        let mut remaining = self.limits.max_directory_entries;
        for (path, weight) in weights {
            check(cancel)?;
            let mut prefix = Vec::new();
            charge_scans(&mut remaining, counts[&prefix], weight)?;
            for component in parent(&path)
                .split(|b| *b == b'/')
                .filter(|part| !part.is_empty())
            {
                if !prefix.is_empty() {
                    prefix.push(b'/');
                }
                prefix.extend_from_slice(component);
                charge_scans(&mut remaining, counts[&prefix], weight)?;
            }
        }
        Ok(())
    }

    pub(super) fn verify(&mut self, cancel: &AtomicBool) -> Result<()> {
        let mut budget = self.limits.max_directory_entries;
        let paths: Vec<_> = self.leaves.keys().cloned().collect();
        for path in paths {
            self.verify_leaf(&path, &mut budget, cancel)?;
        }
        Ok(())
    }

    pub(super) fn verify_leaf(
        &mut self,
        path: &[u8],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let fd = self.parent(path, false, budget, cancel)?;
        let current = fd
            .as_ref()
            .map(|fd| optional_stat(fd, name(path), path))
            .transpose()?
            .flatten();
        match (self.leaves[path].as_ref(), current.as_ref()) {
            (None, None) => Ok(()),
            (Some(before), Some(after)) => {
                same(path, &before.stat, after)?;
                let bytes = crate::checkout::read_leaf(
                    fd.as_ref().unwrap(),
                    path,
                    after,
                    self.limits.content.max_input_bytes,
                    cancel,
                )?;
                if bytes != before.bytes {
                    return Err(refused(path, "file bytes changed since planning"));
                }
                Ok(())
            }
            _ => Err(refused(path, "file presence changed since planning")),
        }
    }

    /// Reopens each ancestor from the absolute root without following links. Missing planned
    /// directories return None; an observed replacement always fails, even under the same name.
    pub(super) fn parent(
        &mut self,
        path: &[u8],
        planning: bool,
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<Option<OwnedFd>> {
        check(cancel)?;
        let mut fd = open_root(&self.root)?;
        identity(
            b"",
            self.directories[b"".as_slice()].as_ref().unwrap(),
            &fstat(&fd).map_err(|e| io(b"", e))?,
        )?;
        let components: Vec<_> = path.split(|b| *b == b'/').collect();
        let mut prefix = Vec::new();
        let mut absent = false;
        for component in &components[..components.len() - 1] {
            let parent = prefix.clone();
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(component);
            let current = if absent {
                None
            } else {
                self.names(&fd, &parent, component, budget, cancel)?;
                optional_stat(&fd, component, &prefix)?
            };
            if !self.directories.contains_key(&prefix) {
                if !planning {
                    return Err(refused(&prefix, "unplanned ancestor"));
                }
                if self.directories.len() >= self.limits.max_directories {
                    return Err(WorktreeError::Limit("directories"));
                }
                if prefix.len() > self.limits.max_path_bytes.saturating_sub(self.path_bytes) {
                    return Err(WorktreeError::Limit("path bytes"));
                }
                self.path_bytes += prefix.len();
                self.directories.insert(prefix.clone(), current);
            }
            match (self.directories[&prefix].as_ref(), current.as_ref()) {
                (None, None) => {
                    absent = true;
                }
                (Some(before), Some(after)) => {
                    if FileType::from_raw_mode(after.st_mode) != FileType::Directory {
                        return Err(refused(&prefix, "ancestor is not a real directory"));
                    }
                    identity(&prefix, before, after)?;
                    fd = openat(
                        &fd,
                        OsStr::from_bytes(component),
                        directory_flags(),
                        rustix::fs::Mode::empty(),
                    )
                    .map_err(|e| io(&prefix, e))?;
                    identity(&prefix, before, &fstat(&fd).map_err(|e| io(&prefix, e))?)?;
                }
                _ => return Err(refused(&prefix, "ancestor changed since planning")),
            }
        }
        if absent {
            return Ok(None);
        }
        self.names(&fd, &prefix, components.last().unwrap(), budget, cancel)?;
        Ok(Some(fd))
    }

    fn names(
        &self,
        fd: &OwnedFd,
        path: &[u8],
        wanted: &[u8],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<Vec<u8>>> {
        if !path.is_empty() && repository_marker(fd, path)? {
            return Err(refused(path, "nested repository marker"));
        }
        let wanted_key = self.policy.key(wanted);
        let mut names = Vec::new();
        for entry in Dir::read_from(fd).map_err(|e| io(path, e))? {
            check(cancel)?;
            let entry = entry.map_err(|e| io(path, e))?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            charge(budget, 1, "directory inspection entries")?;
            if name.len() > 255 {
                return Err(WorktreeError::Limit("directory name bytes"));
            }
            if !wanted.is_empty() && name != wanted && self.policy.key(name) == wanted_key {
                return Err(refused(path, "existing name aliases planned path"));
            }
            names.push(name.to_vec());
        }
        if !path.is_empty() {
            marker(path, &names)?;
        }
        Ok(names)
    }

    fn check_projection(
        &mut self,
        files: &[WorktreeFile],
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let mut additions: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
        for file in files {
            if file.content.is_some() {
                additions
                    .entry(parent(&file.path).to_vec())
                    .or_default()
                    .push(name(&file.path).to_vec());
            }
        }
        for child in self.directories.keys().filter(|path| !path.is_empty()) {
            additions
                .entry(parent(child).to_vec())
                .or_default()
                .push(name(child).to_vec());
        }
        let paths: Vec<_> = self
            .directories
            .keys()
            .filter(|path| !path.is_empty())
            .cloned()
            .collect();
        for directory in paths {
            let probe = join(&directory, b"");
            let mut names = if let Some(fd) = self.parent(&probe, false, budget, cancel)? {
                self.names(&fd, &directory, b"", budget, cancel)?
            } else {
                Vec::new()
            };
            names.retain(|name| !self.removals.contains(&join(&directory, name)));
            names.extend(additions.remove(&directory).unwrap_or_default());
            marker(&directory, &names)?;
        }
        Ok(())
    }
}

fn weight(weights: &mut BTreeMap<Vec<u8>, usize>, path: &[u8], amount: usize) -> Result<()> {
    let entry = weights.entry(path.to_vec()).or_default();
    *entry = entry.checked_add(amount).ok_or(WorktreeError::Limit(
        "publication directory inspection entries",
    ))?;
    Ok(())
}
fn increment(counts: &mut BTreeMap<Vec<u8>, usize>, path: &[u8]) -> Result<()> {
    let entry = counts.get_mut(path).unwrap();
    *entry = entry.checked_add(1).ok_or(WorktreeError::Limit(
        "publication directory inspection entries",
    ))?;
    Ok(())
}
fn charge_scans(remaining: &mut usize, count: usize, weight: usize) -> Result<()> {
    let total = count.checked_mul(weight).ok_or(WorktreeError::Limit(
        "publication directory inspection entries",
    ))?;
    charge(remaining, total, "publication directory inspection entries")
}

pub(super) fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(WorktreeError::Cancelled)
    } else {
        Ok(())
    }
}
pub(super) fn charge(remaining: &mut usize, amount: usize, label: &'static str) -> Result<()> {
    if amount > *remaining {
        return Err(WorktreeError::Limit(label));
    }
    *remaining -= amount;
    Ok(())
}
pub(super) fn identity(path: &[u8], before: &Stat, after: &Stat) -> Result<()> {
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_mode != after.st_mode
    {
        return Err(refused(path, "directory or file identity changed"));
    }
    Ok(())
}
fn marker(path: &[u8], names: &[Vec<u8>]) -> Result<()> {
    let has = |wanted: &[u8]| names.iter().any(|name| name.eq_ignore_ascii_case(wanted));
    if has(b".git") || (has(b"HEAD") && has(b"objects") && has(b"refs")) {
        return Err(refused(path, "nested repository marker"));
    }
    Ok(())
}
pub(super) fn name(path: &[u8]) -> &[u8] {
    path.rsplit(|b| *b == b'/').next().unwrap_or_default()
}
pub(super) fn parent(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|b| *b == b'/')
        .map_or(b"", |at| &path[..at])
}
pub(super) fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = parent.to_vec();
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}
pub(super) fn refused(path: &[u8], reason: &'static str) -> WorktreeError {
    WorktreeError::Refused {
        path: path.to_vec(),
        reason,
    }
}
pub(super) fn io(path: &[u8], source: impl Into<std::io::Error>) -> WorktreeError {
    WorktreeError::Io {
        path: path.to_vec(),
        source: source.into(),
    }
}

/// Publication records each successful syscall that mutates a path before any later fallible work.
pub(super) fn publish(
    plan: PreparedWorktree,
    cancel: &AtomicBool,
    hook: &mut impl FnMut(&str, &[u8]) -> Result<()>,
) -> std::result::Result<super::WorktreeReport, super::WorktreeFailure> {
    use std::io::Write;

    use rustix::fs::{AtFlags, Mode, OFlags, fchmod, mkdirat, unlinkat};

    use super::{WorktreeAction, WorktreeReport, WorktreeWrite};

    let PreparedWorktree {
        files,
        mut snapshot,
    } = plan;
    let mut report = WorktreeReport::default();
    let result: Result<()> = (|| {
        snapshot.verify(cancel)?;
        snapshot.admit_publication(&files, cancel)?;
        hook("preflight complete", b"")?;
        let mut budget = snapshot.limits.max_directory_entries;
        let missing: Vec<_> = snapshot
            .directories
            .iter()
            .filter(|(_, stat)| stat.is_none())
            .map(|(path, _)| path.clone())
            .collect();
        for path in missing {
            hook("before directory", &path)?;
            check(cancel)?;
            let fd = snapshot
                .parent(&path, false, &mut budget, cancel)?
                .ok_or_else(|| refused(&path, "directory parent is absent"))?;
            if optional_stat(&fd, name(&path), &path)?.is_some() {
                return Err(refused(&path, "directory appeared after planning"));
            }
            mkdirat(
                &fd,
                OsStr::from_bytes(name(&path)),
                Mode::from_raw_mode(0o755),
            )
            .map_err(|e| io(&path, e))?;
            report.writes.push(WorktreeWrite {
                path: path.clone(),
                action: WorktreeAction::DirectoryCreated,
                complete: true,
            });
            let opened = openat(
                &fd,
                OsStr::from_bytes(name(&path)),
                directory_flags(),
                Mode::empty(),
            )
            .map_err(|e| io(&path, e))?;
            let stat = fstat(&opened).map_err(|e| io(&path, e))?;
            hook("after directory", &path)?;
            let named = optional_stat(&fd, name(&path), &path)?
                .ok_or_else(|| refused(&path, "new directory disappeared"))?;
            identity(&path, &stat, &named)?;
            snapshot.directories.insert(path, Some(stat));
        }
        let removals: Vec<_> = snapshot.removals.iter().cloned().collect();
        for path in removals {
            hook("before removal", &path)?;
            check(cancel)?;
            snapshot.verify_leaf(&path, &mut budget, cancel)?;
            let fd = snapshot
                .parent(&path, false, &mut budget, cancel)?
                .ok_or_else(|| refused(&path, "source parent disappeared"))?;
            let current = optional_stat(&fd, name(&path), &path)?
                .ok_or_else(|| refused(&path, "source disappeared"))?;
            same(
                &path,
                &snapshot.leaves[&path].as_ref().unwrap().stat,
                &current,
            )?;
            unlinkat(&fd, OsStr::from_bytes(name(&path)), AtFlags::empty())
                .map_err(|e| io(&path, e))?;
            report.writes.push(WorktreeWrite {
                path: path.clone(),
                action: WorktreeAction::Removed,
                complete: true,
            });
            snapshot.leaves.insert(path.clone(), None);
            hook("after removal", &path)?;
        }
        let mut installed = BTreeMap::new();
        for file in &files {
            let Some(bytes) = &file.content else {
                continue;
            };
            hook("before install", &file.path)?;
            check(cancel)?;
            if let Some(source) = &file.source_path
                && !snapshot.removals.contains(source)
            {
                snapshot.verify_leaf(source, &mut budget, cancel)?;
            }
            snapshot.verify_leaf(&file.path, &mut budget, cancel)?;
            let fd = snapshot
                .parent(&file.path, false, &mut budget, cancel)?
                .ok_or_else(|| refused(&file.path, "destination parent disappeared"))?;
            let mode = Mode::from_raw_mode(if file.executable { 0o755 } else { 0o644 });
            let opened = openat(
                &fd,
                OsStr::from_bytes(name(&file.path)),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                mode,
            )
            .map_err(|e| io(&file.path, e))?;
            report.writes.push(WorktreeWrite {
                path: file.path.clone(),
                action: WorktreeAction::Created,
                complete: false,
            });
            hook("after create", &file.path)?;
            let owned = fstat(&opened).map_err(|e| io(&file.path, e))?;
            let mut output = std::fs::File::from(opened);
            hook("before write", &file.path)?;
            let fresh = snapshot
                .parent(&file.path, false, &mut budget, cancel)?
                .ok_or_else(|| refused(&file.path, "destination parent changed"))?;
            let named = optional_stat(&fresh, name(&file.path), &file.path)?
                .ok_or_else(|| refused(&file.path, "created file disappeared"))?;
            identity(&file.path, &owned, &named)?;
            for chunk in bytes.chunks(64 * 1024) {
                check(cancel)?;
                let named = optional_stat(&fresh, name(&file.path), &file.path)?
                    .ok_or_else(|| refused(&file.path, "created file disappeared"))?;
                identity(&file.path, &owned, &named)?;
                output.write_all(chunk).map_err(|e| io(&file.path, e))?;
                hook("after write chunk", &file.path)?;
            }
            check(cancel)?;
            fchmod(&output, mode).map_err(|e| io(&file.path, e))?;
            let stat = fstat(&output).map_err(|e| io(&file.path, e))?;
            installed.insert(file.path.clone(), stat);
            report.writes.last_mut().unwrap().complete = true;
            hook("after install", &file.path)?;
        }
        hook("before final verification", b"")?;
        for file in &files {
            let Some(bytes) = &file.content else {
                continue;
            };
            let fd = snapshot
                .parent(&file.path, false, &mut budget, cancel)?
                .ok_or_else(|| refused(&file.path, "installed parent disappeared"))?;
            let current = optional_stat(&fd, name(&file.path), &file.path)?
                .ok_or_else(|| refused(&file.path, "installed file disappeared"))?;
            same(&file.path, &installed[&file.path], &current)?;
            let actual = crate::checkout::read_leaf(
                &fd,
                &file.path,
                &current,
                snapshot.limits.content.max_output_bytes,
                cancel,
            )?;
            if actual != *bytes {
                return Err(refused(&file.path, "installed file bytes changed"));
            }
        }
        let remaining: Vec<_> = snapshot
            .leaves
            .keys()
            .filter(|path| !installed.contains_key(*path))
            .cloned()
            .collect();
        for path in remaining {
            snapshot.verify_leaf(&path, &mut budget, cancel)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(report),
        Err(cause) => Err(super::WorktreeFailure {
            cause: Box::new(cause),
            report,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::WorktreeAction;
    use crate::attributes::{Attributes, Case, Limits as AttributeLimits};
    use crate::filter::{Drivers, Pipeline, Settings};
    use crate::{InitKind, ObjectFormat};

    fn setup() -> (tempfile::TempDir, Repository, Pipeline) {
        let temporary = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            ObjectFormat::Sha1,
            temporary.path().join("r"),
            InitKind::Worktree,
        )
        .unwrap();
        std::fs::write(repo.worktree().unwrap().join("file"), b"old\n").unwrap();
        let filters = Pipeline::from_attributes(
            Attributes::new(Case::Sensitive, AttributeLimits::default()),
            Settings::default(),
            Drivers::default(),
        );
        (temporary, repo, filters)
    }
    const CHANGE: &[u8] = b"--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";

    #[test]
    fn failures_at_every_boundary_report_exact_confirmed_mutations() {
        for stage in [
            "preflight complete",
            "before removal",
            "after removal",
            "before install",
            "after create",
            "before write",
            "after install",
            "before final verification",
        ] {
            let (_temporary, repo, mut filters) = setup();
            let plan = Patch::parse(CHANGE)
                .unwrap()
                .prepare_worktree(&repo, &mut filters)
                .unwrap();
            let failure = publish(plan, &AtomicBool::new(false), &mut |at, _| {
                if at == stage {
                    Err(refused(b"file", "injected failure"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            let file = repo.worktree().unwrap().join("file");
            match stage {
                "preflight complete" | "before removal" => {
                    assert!(failure.report.writes.is_empty());
                    assert_eq!(std::fs::read(file).unwrap(), b"old\n");
                }
                "after removal" | "before install" => {
                    assert_eq!(failure.report.writes.len(), 1);
                    assert_eq!(failure.report.writes[0].action, WorktreeAction::Removed);
                    assert!(!file.exists());
                }
                "after create" | "before write" => {
                    assert_eq!(failure.report.writes.len(), 2);
                    assert_eq!(failure.report.writes[1].action, WorktreeAction::Created);
                    assert!(!failure.report.writes[1].complete);
                    assert_eq!(std::fs::read(file).unwrap(), b"");
                }
                _ => {
                    assert_eq!(failure.report.writes.len(), 2);
                    assert!(failure.report.writes[1].complete);
                    assert_eq!(std::fs::read(file).unwrap(), b"new\n");
                }
            }
        }
    }

    #[test]
    fn raced_obstruction_is_retained_and_not_reported_as_our_creation() {
        let (_temporary, repo, mut filters) = setup();
        let plan = Patch::parse(CHANGE)
            .unwrap()
            .prepare_worktree(&repo, &mut filters)
            .unwrap();
        let file = repo.worktree().unwrap().join("file");
        let failure = publish(plan, &AtomicBool::new(false), &mut |stage, _| {
            if stage == "before install" {
                std::fs::write(&file, b"foreign").unwrap();
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(failure.report.writes.len(), 1);
        assert_eq!(failure.report.writes[0].action, WorktreeAction::Removed);
        assert_eq!(std::fs::read(file).unwrap(), b"foreign");
    }

    #[test]
    fn symlink_replacement_after_creation_never_writes_the_target() {
        let (_temporary, repo, mut filters) = setup();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"outside").unwrap();
        let plan = Patch::parse(CHANGE)
            .unwrap()
            .prepare_worktree(&repo, &mut filters)
            .unwrap();
        let file = repo.worktree().unwrap().join("file");
        let failure = publish(plan, &AtomicBool::new(false), &mut |stage, _| {
            if stage == "after create" {
                std::fs::remove_file(&file).unwrap();
                std::os::unix::fs::symlink(outside.path(), &file).unwrap();
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(failure.report.writes.len(), 2);
        assert!(!failure.report.writes[1].complete);
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"outside");
    }

    #[test]
    fn changed_second_source_after_preflight_is_refused_before_its_removal() {
        let (_temporary, repo, mut filters) = setup();
        let later = repo.worktree().unwrap().join("later");
        std::fs::write(&later, b"old\n").unwrap();
        let bytes = [
            CHANGE,
            b"--- a/later\n+++ b/later\n@@ -1 +1 @@\n-old\n+new\n",
        ]
        .concat();
        let plan = Patch::parse(&bytes)
            .unwrap()
            .prepare_worktree(&repo, &mut filters)
            .unwrap();
        let failure = publish(plan, &AtomicBool::new(false), &mut |stage, path| {
            if stage == "before removal" && path == b"later" {
                std::fs::write(&later, b"external").unwrap();
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(failure.report.writes.len(), 1);
        assert_eq!(failure.report.writes[0].path, b"file");
        assert_eq!(failure.report.writes[0].action, WorktreeAction::Removed);
        assert_eq!(std::fs::read(later).unwrap(), b"external");
    }

    #[test]
    fn replaced_new_directory_is_detected_before_any_child_write() {
        let (_temporary, repo, mut filters) = setup();
        let patch = Patch::parse(b"--- /dev/null\n+++ b/new/file\n@@ -0,0 +1 @@\n+new\n").unwrap();
        let plan = patch.prepare_worktree(&repo, &mut filters).unwrap();
        let directory = repo.worktree().unwrap().join("new");
        let moved = repo.worktree().unwrap().join("moved");
        let failure = publish(plan, &AtomicBool::new(false), &mut |stage, _| {
            if stage == "after directory" {
                std::fs::rename(&directory, &moved).unwrap();
                std::fs::create_dir(&directory).unwrap();
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(failure.report.writes.len(), 1);
        assert_eq!(
            failure.report.writes[0].action,
            WorktreeAction::DirectoryCreated
        );
        assert!(!directory.join("file").exists());
        assert!(!moved.join("file").exists());
    }
    #[test]
    fn partial_write_is_retained_with_an_incomplete_report_entry() {
        let (_temporary, repo, mut filters) = setup();
        let mut bytes = b"--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', 128 * 1024));
        bytes.push(b'\n');
        let plan = Patch::parse(&bytes)
            .unwrap()
            .prepare_worktree(&repo, &mut filters)
            .unwrap();
        let failure = publish(plan, &AtomicBool::new(false), &mut |stage, _| {
            if stage == "after write chunk" {
                Err(refused(b"file", "injected write failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(failure.report.writes.len(), 2);
        assert_eq!(failure.report.writes[1].action, WorktreeAction::Created);
        assert!(!failure.report.writes[1].complete);
        assert_eq!(
            std::fs::read(repo.worktree().unwrap().join("file"))
                .unwrap()
                .len(),
            64 * 1024
        );
    }
}
