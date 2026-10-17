use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::atomic::AtomicBool;

use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, openat, statat};

use super::types::{charge, check, value};
use super::{
    Change, Error, Limits, Untracked, UntrackedFiles, UntrackedKind, UntrackedPath, WorktreeChange,
};
use crate::ignore::{Ignore, InputKind, Source};
use crate::{EntryMode, ObjectId, Repository, TreeValue, index};

pub(super) struct Observation {
    pub changes: Vec<WorktreeChange>,
    pub untracked: Vec<Vec<u8>>,
    pub listed: Vec<UntrackedPath>,
    pub boundaries: Vec<Vec<u8>>,
}

/// How a scan treats paths absent from the index.
pub(super) enum Listing {
    /// Raw status: list every leaf or none, never consulting ignore rules.
    Raw(Untracked),
    /// Ignore-aware status: these rules plus each visited directory's `.gitignore`.
    Ignoring(Box<Ignore>, UntrackedFiles),
}

pub(super) fn scan(
    repo: &Repository,
    root: &Path,
    entries: &[index::Entry],
    listing: Listing,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Observation, Error> {
    scan_with_hook(repo, root, entries, listing, limits, cancel, &mut |_| {})
}

// A private checkpoint permits deterministic race/cancellation tests without timing assumptions.
fn scan_with_hook(
    repo: &Repository,
    root: &Path,
    entries: &[index::Entry],
    listing: Listing,
    limits: Limits,
    cancel: &AtomicBool,
    after_read: &mut dyn FnMut(&[u8]),
) -> Result<Observation, Error> {
    let root_fd = open_root(root)?;
    let mut metadata = Vec::new();
    for path in [repo.git_dir(), repo.common_dir(), repo.object_dir()] {
        let stat = rustix::fs::stat(path).map_err(|source| io(b"", source))?;
        metadata.push(stat);
    }
    let mut scan = Scan {
        format: repo.object_format(),
        entries: entries
            .iter()
            .map(|entry| (entry.path.as_slice(), entry))
            .collect(),
        needed: BTreeSet::new(),
        observations: BTreeMap::new(),
        untracked: Vec::new(),
        listed: Vec::new(),
        boundaries: Vec::new(),
        metadata,
        listing,
        limits,
        cancel,
        after_read,
    };
    for entry in entries {
        check(cancel)?;
        validate_path(&entry.path)?;
        if entry.path.iter().filter(|byte| **byte == b'/').count() > scan.limits.max_depth.min(128)
        {
            return Err(Error::Limit("directory depth"));
        }
        for (offset, byte) in entry.path.iter().enumerate() {
            if *byte == b'/' && !scan.needed.contains(&entry.path[..offset]) {
                charge(&mut scan.limits.max_path_bytes, offset, "path bytes")?;
                scan.needed.insert(entry.path[..offset].to_vec());
            }
        }
    }
    scan.directory(&root_fd, b"", 0, Walk::default())?;
    let reopened = open_root(root)?;
    let original = fstat(&root_fd).map_err(|source| io(b"", source))?;
    let current = fstat(&reopened).map_err(|source| io(b"", source))?;
    same(b"", &original, &current)?;
    let mut changes = Vec::new();
    for entry in entries {
        check(cancel)?;
        if entry.stage != index::Stage::Normal || entry.mode == index::Mode::Gitlink {
            continue;
        }
        if let Some(change) = difference(entry, &scan.observations) {
            changes.push(WorktreeChange {
                path: entry.path.clone(),
                index: value(entry),
                change,
            });
        }
    }
    scan.untracked.sort_unstable();
    scan.listed.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    scan.boundaries.sort_unstable();
    check(cancel)?;
    Ok(Observation {
        changes,
        untracked: scan.untracked,
        listed: scan.listed,
        boundaries: scan.boundaries,
    })
}

/// Ignore state inherited by a directory's children.
#[derive(Clone, Copy, Default)]
struct Walk {
    /// An ancestor is ignored, so every untracked descendant is too.
    excluded: bool,
    /// Inside a collapsed untracked directory: the `listed` length when it was entered.
    collapse: Option<usize>,
}

/// What [`Scan::directory`] found at a directory path.
#[derive(PartialEq)]
enum Visited {
    Directory,
    /// A nested repository; never entered.
    Repository,
    /// This repository's own metadata directory; never entered.
    Metadata,
}

#[derive(Clone, Copy)]
enum Observed {
    Leaf(TreeValue),
    Directory,
    Blocked,
}

struct Scan<'a> {
    format: crate::ObjectFormat,
    entries: BTreeMap<&'a [u8], &'a index::Entry>,
    needed: BTreeSet<Vec<u8>>,
    observations: BTreeMap<Vec<u8>, Observed>,
    untracked: Vec<Vec<u8>>,
    listed: Vec<UntrackedPath>,
    boundaries: Vec<Vec<u8>>,
    metadata: Vec<Stat>,
    listing: Listing,
    limits: Limits,
    cancel: &'a AtomicBool,
    after_read: &'a mut dyn FnMut(&[u8]),
}

impl Scan<'_> {
    /// Whether untracked paths are listed at all.
    fn lists_untracked(&self) -> bool {
        match &self.listing {
            Listing::Raw(policy) => *policy == Untracked::RawFilesWithoutIgnores,
            Listing::Ignoring(_, files) => *files != UntrackedFiles::Omit,
        }
    }

    /// Whether ignore rules exclude this path, given that no ancestor is excluded.
    fn ignored(&self, path: &[u8], kind: InputKind) -> Result<bool, Error> {
        let Listing::Ignoring(rules, _) = &self.listing else {
            return Ok(false);
        };
        let found = rules
            .check_direct(path, kind)
            .map_err(|source| Error::Ignore {
                path: path.to_vec(),
                source,
            })?;
        Ok(found.is_some_and(|found| found.ignored))
    }

    /// Adds this directory's `.gitignore`. Like Git, a symlink or non-file is skipped.
    fn load_gitignore(&mut self, fd: &OwnedFd, path: &[u8]) -> Result<(), Error> {
        let mut file = path.to_vec();
        if !file.is_empty() {
            file.push(b'/');
        }
        file.extend_from_slice(b".gitignore");
        let Some(stat) = optional_stat(fd, b".gitignore", &file)? else {
            return Ok(());
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok(());
        }
        let max = self
            .limits
            .max_file_bytes
            .min(self.limits.max_worktree_bytes);
        let content = self.read_file(fd, b".gitignore", &file, &stat, max)?;
        charge(
            &mut self.limits.max_worktree_bytes,
            content.len(),
            "worktree bytes",
        )?;
        if let Listing::Ignoring(rules, _) = &mut self.listing {
            rules
                .add(Source::Directory(path), &content)
                .map_err(|source| Error::Ignore { path: file, source })?;
        }
        Ok(())
    }

    fn directory(
        &mut self,
        fd: &OwnedFd,
        path: &[u8],
        depth: usize,
        walk: Walk,
    ) -> Result<Visited, Error> {
        check(self.cancel)?;
        let before = fstat(fd).map_err(|source| io(path, source))?;
        if self
            .metadata
            .iter()
            .any(|metadata| metadata.st_dev == before.st_dev && metadata.st_ino == before.st_ino)
        {
            self.boundaries.push(path.to_vec());
            self.observations.insert(path.to_vec(), Observed::Blocked);
            return Ok(Visited::Metadata);
        }
        // Detect nested repositories before entering any of their children. Merely lstat the
        // marker; never open it. The root marker is expected and ignored below.
        if !path.is_empty() && repository_marker(fd, path)? {
            self.boundaries.push(path.to_vec());
            self.observations.insert(path.to_vec(), Observed::Blocked);
            return Ok(Visited::Repository);
        }
        self.observations.insert(path.to_vec(), Observed::Directory);
        let ignoring = matches!(self.listing, Listing::Ignoring(..));
        if ignoring && !walk.excluded && self.lists_untracked() {
            self.load_gitignore(fd, path)?;
        }
        let directory = Dir::read_from(fd).map_err(|source| io(path, source))?;
        for entry in directory {
            check(self.cancel)?;
            // A collapsed directory needs only one visible untracked path.
            if walk.collapse.is_some_and(|mark| self.listed.len() > mark) {
                break;
            }
            let entry = entry.map_err(|source| io(path, source))?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            charge(
                &mut self.limits.max_directory_entries,
                1,
                "directory entries",
            )?;
            let length = path
                .len()
                .checked_add(name.len())
                .and_then(|n| n.checked_add(usize::from(!path.is_empty())))
                .ok_or(Error::Limit("path bytes"))?;
            charge(&mut self.limits.max_path_bytes, length, "path bytes")?;
            let mut child = path.to_vec();
            if !child.is_empty() {
                child.push(b'/');
            }
            child.extend_from_slice(name);
            if name.eq_ignore_ascii_case(b".git") {
                if name != b".git" {
                    return Err(unsupported(&child, "metadata name alias"));
                }
                continue;
            }
            // Validate bytes before constructing an OS string or opening a directory entry.
            validate_path(&child)?;
            let tracked = self.entries.contains_key(child.as_slice());
            let needed = self.needed.contains(&child);
            if !tracked && !needed && (!self.lists_untracked() || walk.excluded) {
                continue;
            }
            let gitlink = self
                .entries
                .get(child.as_slice())
                .is_some_and(|entry| entry.mode == index::Mode::Gitlink);
            if gitlink {
                continue;
            }
            let stat = statat(fd, OsStr::from_bytes(name), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|source| io(&child, source))?;
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind == FileType::Directory {
                if tracked && !needed && (!self.lists_untracked() || walk.excluded) {
                    self.observations.insert(child, Observed::Directory);
                    continue;
                }
                let mut inner = walk;
                if ignoring {
                    let ignored = walk.excluded || self.ignored(&child, InputKind::Directory)?;
                    if needed {
                        inner.excluded = ignored;
                    } else {
                        let collapsed = matches!(
                            self.listing,
                            Listing::Ignoring(_, UntrackedFiles::Collapsed)
                        );
                        // Git's collapsed listing hides a directory at a tracked file's path.
                        if ignored || (tracked && collapsed) {
                            if tracked {
                                self.observations.insert(child, Observed::Directory);
                            }
                            continue;
                        }
                        if collapsed && inner.collapse.is_none() {
                            inner.collapse = Some(self.listed.len());
                        }
                    }
                }
                if depth >= self.limits.max_depth.min(128) {
                    return Err(Error::Limit("directory depth"));
                }
                let child_fd = openat(
                    fd,
                    OsStr::from_bytes(name),
                    directory_flags(),
                    Mode::empty(),
                )
                .map_err(|source| io(&child, source))?;
                let opened = fstat(&child_fd).map_err(|source| io(&child, source))?;
                same(&child, &stat, &opened)?;
                let visited = self.directory(&child_fd, &child, depth + 1, inner)?;
                let after = statat(fd, OsStr::from_bytes(name), AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|source| io(&child, source))?;
                same(&child, &stat, &after)?;
                if ignoring && !needed {
                    self.list_directory(child, visited, walk, inner);
                }
            } else {
                self.leaf(fd, name, &child, &stat, tracked)?;
            }
        }
        let after = fstat(fd).map_err(|source| io(path, source))?;
        same(path, &before, &after)?;
        Ok(Visited::Directory)
    }

    /// Lists a visible untracked directory after its traversal.
    fn list_directory(&mut self, path: Vec<u8>, visited: Visited, outer: Walk, inner: Walk) {
        let kind = match visited {
            Visited::Repository => UntrackedKind::Repository,
            Visited::Metadata => return,
            Visited::Directory => {
                // Only the outermost collapsed directory replaces what was found inside it.
                let Some(mark) = inner.collapse.filter(|_| outer.collapse.is_none()) else {
                    return;
                };
                if self.listed.len() == mark {
                    return;
                }
                self.listed.truncate(mark);
                UntrackedKind::Directory
            }
        };
        self.listed.push(UntrackedPath { path, kind });
    }

    fn leaf(
        &mut self,
        fd: &OwnedFd,
        name: &[u8],
        path: &[u8],
        stat: &Stat,
        tracked: bool,
    ) -> Result<(), Error> {
        let kind = FileType::from_raw_mode(stat.st_mode);
        if !tracked {
            self.observations.insert(path.to_vec(), Observed::Blocked);
            match self.listing {
                Listing::Raw(Untracked::RawFilesWithoutIgnores) => {
                    self.untracked.push(path.to_vec());
                }
                // Git lists only regular files and symlinks, never sockets or FIFOs.
                Listing::Ignoring(_, files)
                    if files != UntrackedFiles::Omit
                        && matches!(kind, FileType::RegularFile | FileType::Symlink)
                        && !self.ignored(path, InputKind::File)? =>
                {
                    self.listed.push(UntrackedPath {
                        path: path.to_vec(),
                        kind: UntrackedKind::File,
                    });
                }
                _ => (),
            }
            return Ok(());
        }
        let mode = match kind {
            FileType::RegularFile if stat.st_mode & 0o100 != 0 => EntryMode::Executable,
            FileType::RegularFile => EntryMode::Blob,
            FileType::Symlink => EntryMode::Symlink,
            _ => {
                self.observations.insert(path.to_vec(), Observed::Blocked);
                return Ok(());
            }
        };
        let max = self
            .limits
            .max_file_bytes
            .min(self.limits.max_worktree_bytes);
        let bytes = if kind == FileType::Symlink {
            // One extra byte distinguishes an exact limit from truncation. Cap by the platform's
            // path bound as well, avoiding an oversized allocation for a caller's huge limit.
            let capacity = max
                .min(1024 * 1024)
                .checked_add(1)
                .ok_or(Error::Limit("file bytes"))?;
            let mut bytes = vec![0; capacity];
            let count =
                rustix::fs::readlinkat_raw(fd, OsStr::from_bytes(name), bytes.as_mut_slice())
                    .map_err(|source| io(path, source))?;
            self.check_bytes(count)?;
            if count == capacity {
                return Err(Error::Limit("file bytes"));
            }
            bytes.truncate(count);
            bytes
        } else {
            self.read_file(fd, name, path, stat, max)?
        };
        charge(
            &mut self.limits.max_worktree_bytes,
            bytes.len(),
            "worktree bytes",
        )?;
        (self.after_read)(path);
        check(self.cancel)?;
        let after = statat(fd, OsStr::from_bytes(name), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| io(path, source))?;
        same(path, stat, &after)?;
        self.observations.insert(
            path.to_vec(),
            Observed::Leaf(TreeValue {
                id: ObjectId::for_blob(self.format, &bytes),
                mode,
            }),
        );
        Ok(())
    }

    /// Keep the per-file ceiling distinct from the remaining aggregate read budget.
    fn check_bytes(&self, size: usize) -> Result<(), Error> {
        if size > self.limits.max_file_bytes {
            return Err(Error::Limit("file bytes"));
        }
        if size > self.limits.max_worktree_bytes {
            return Err(Error::Limit("worktree bytes"));
        }
        Ok(())
    }

    fn read_file(
        &self,
        fd: &OwnedFd,
        name: &[u8],
        path: &[u8],
        stat: &Stat,
        max: usize,
    ) -> Result<Vec<u8>, Error> {
        let opened = openat(
            fd,
            OsStr::from_bytes(name),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|source| io(path, source))?;
        let before = fstat(&opened).map_err(|source| io(path, source))?;
        same(path, stat, &before)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
            return Err(Error::Changed(path.to_vec()));
        }
        let size = usize::try_from(before.st_size).map_err(|_| Error::Limit("file bytes"))?;
        self.check_bytes(size)?;
        let mut file = File::from(opened);
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check(self.cancel)?;
            let remaining = max - bytes.len();
            let count = file
                .read(&mut buffer[..remaining.saturating_add(1).min(64 * 1024)])
                .map_err(|source| io(path, source))?;
            self.check_bytes(bytes.len() + count)?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        let after = fstat(&file).map_err(|source| io(path, source))?;
        same(path, &before, &after)?;
        if bytes.len() != size {
            return Err(Error::Changed(path.to_vec()));
        }
        Ok(bytes)
    }
}

fn difference(entry: &index::Entry, actual: &BTreeMap<Vec<u8>, Observed>) -> Option<Change> {
    for (offset, byte) in entry.path.iter().enumerate() {
        if *byte != b'/' {
            continue;
        }
        let path = &entry.path[..offset];
        match actual.get(path) {
            Some(Observed::Directory) => (),
            Some(_) => {
                return Some(Change::Obstructed {
                    path: path.to_vec(),
                });
            }
            None => return Some(Change::Deleted),
        }
    }
    match actual.get(&entry.path) {
        Some(Observed::Leaf(actual)) if *actual == value(entry) => None,
        Some(Observed::Leaf(actual)) => Some(Change::Modified(*actual)),
        Some(_) => Some(Change::Obstructed {
            path: entry.path.clone(),
        }),
        None => Some(Change::Deleted),
    }
}

pub(crate) fn validate_path(path: &[u8]) -> Result<(), Error> {
    if path.is_empty()
        || path.contains(&0)
        || path.contains(&b'\\')
        || path.contains(&b':')
        || path.split(|byte| *byte == b'/').any(|part| {
            part.is_empty() || part == b"." || part == b".." || part.eq_ignore_ascii_case(b".git")
        })
    {
        return Err(unsupported(path, "unsafe or platform-ambiguous path"));
    }
    if cfg!(target_os = "macos") && !path.is_ascii() {
        return Err(unsupported(path, "non-ASCII filesystem paths on macOS"));
    }
    Ok(())
}

pub(crate) fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

// Opening the absolute root component-by-component avoids following a substituted ancestor.
pub(crate) fn open_root(path: &Path) -> Result<OwnedFd, Error> {
    let mut fd = rustix::fs::open("/", directory_flags(), Mode::empty())
        .map_err(|source| io(b"", source))?;
    for component in path.components() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                fd = openat(&fd, name, directory_flags(), Mode::empty())
                    .map_err(|source| io(b"", source))?;
            }
            _ => {
                return Err(unsupported(
                    b"",
                    "working-tree root is not canonical absolute",
                ));
            }
        }
    }
    Ok(fd)
}

pub(crate) fn repository_marker(fd: &OwnedFd, path: &[u8]) -> Result<bool, Error> {
    if optional_stat(fd, b".git", path)?.is_some() {
        return Ok(true);
    }
    // Conservatively recognize a bare repository without reading its metadata.
    Ok(optional_stat(fd, b"HEAD", path)?.is_some()
        && optional_stat(fd, b"objects", path)?.is_some()
        && optional_stat(fd, b"refs", path)?.is_some())
}

pub(crate) fn optional_stat(fd: &OwnedFd, name: &[u8], path: &[u8]) -> Result<Option<Stat>, Error> {
    match statat(fd, OsStr::from_bytes(name), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(source) => Err(io(path, source)),
    }
}

pub(crate) fn same(path: &[u8], before: &Stat, after: &Stat) -> Result<(), Error> {
    if before.st_dev != after.st_dev
        || before.st_ino != after.st_ino
        || before.st_mode != after.st_mode
        || before.st_size != after.st_size
        || before.st_mtime != after.st_mtime
        || before.st_mtime_nsec != after.st_mtime_nsec
        || before.st_ctime != after.st_ctime
        || before.st_ctime_nsec != after.st_ctime_nsec
    {
        Err(Error::Changed(path.to_vec()))
    } else {
        Ok(())
    }
}
fn io(path: &[u8], source: impl Into<std::io::Error>) -> Error {
    Error::Io {
        path: path.to_vec(),
        source: source.into(),
    }
}
fn unsupported(path: &[u8], reason: &'static str) -> Error {
    Error::Unsupported {
        path: path.to_vec(),
        reason,
    }
}

#[cfg(test)]
mod tests;
