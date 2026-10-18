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
            if leaf.stat.st_mode & 0o111 != 0 {
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
            let staged_entry = index.as_ref().and_then(|index| {
                let at = index
                    .entries()
                    .partition_point(|entry| entry.path.as_slice() < path.as_slice());
                index.entries().get(at).filter(|entry| {
                    entry.path == *path
                        && entry.stage == crate::index::Stage::Normal
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
