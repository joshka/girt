use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};

use parallel::{Budget, Open, Queue, Task};
use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, openat, statat};
use tracked::{Slots, Tracked};

use super::types::{charge, check, value};
use super::{
    Error, Limits, Untracked, UntrackedFiles, UntrackedKind, UntrackedPath, WorktreeChange,
};
use crate::ignore::{self, Ignore, InputKind, Source};
use crate::pathspec::{Pathspec, Prune};
use crate::{EntryMode, ObjectId, Repository, TreeValue, index};

pub(super) struct Observation {
    pub changes: Vec<WorktreeChange>,
    pub untracked: Vec<Vec<u8>>,
    pub listed: Vec<UntrackedPath>,
    pub boundaries: Vec<Vec<u8>>,
}

/// Conversion applied to tracked regular files before comparing them with the index.
pub(crate) trait Convert: Sync {
    /// The stored form of `file` at `path`, whose index entry names `staged`.
    fn convert(&self, path: &[u8], file: &[u8], staged: ObjectId) -> Result<Vec<u8>, Error>;
}

/// A shared pipeline with the objects `text=auto` reads staged blobs from.
pub(crate) struct Shared<'a> {
    pub(crate) pipeline: Mutex<&'a mut crate::filter::Pipeline>,
    pub(crate) objects: &'a crate::Objects,
}

impl Convert for Shared<'_> {
    fn convert(&self, path: &[u8], file: &[u8], staged: ObjectId) -> Result<Vec<u8>, Error> {
        let mut pipeline = self
            .pipeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staged = crate::filter::Staged::Object(self.objects, staged);
        pipeline
            .stored_for_status(path, file, staged)
            .map(std::borrow::Cow::into_owned)
            .map_err(|source| Error::Filter {
                path: path.to_vec(),
                source: Box::new(source),
            })
    }
}

pub(crate) type Filters<'a> = &'a dyn Convert;

/// How a scan treats paths absent from the index.
pub(super) enum Listing {
    /// Raw status: list every leaf or none, never consulting ignore rules.
    Raw(Untracked),
    /// Ignore-aware status: these rules plus each visited directory's `.gitignore`.
    Ignoring(Box<Ignore>, UntrackedFiles),
}

/// [`Listing`] without its rules, which travel with each directory instead.
#[derive(Clone, Copy, PartialEq)]
enum Policy {
    Raw(Untracked),
    Ignoring(UntrackedFiles),
}

#[expect(clippy::too_many_arguments, reason = "one traversal's inputs")]
pub(super) fn scan(
    repo: &Repository,
    root: &Path,
    entries: &[index::Entry],
    index_written: Option<u32>,
    listing: Listing,
    paths: &Pathspec,
    filters: Option<Filters<'_>>,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Observation, Error> {
    scan_with_hook(
        repo,
        root,
        entries,
        index_written,
        listing,
        paths,
        filters,
        limits,
        cancel,
        &|_| {},
    )
}

// A private checkpoint permits deterministic race/cancellation tests without timing assumptions.
#[expect(clippy::too_many_arguments, reason = "private test seam for `scan`")]
fn scan_with_hook(
    repo: &Repository,
    root: &Path,
    entries: &[index::Entry],
    index_written: Option<u32>,
    listing: Listing,
    paths: &Pathspec,
    filters: Option<Filters<'_>>,
    limits: Limits,
    cancel: &AtomicBool,
    after_read: &(dyn Fn(&[u8]) + Sync),
) -> Result<Observation, Error> {
    let root_fd = Arc::new(open_root(root)?);
    let mut metadata = Vec::new();
    for path in [repo.git_dir(), repo.common_dir(), repo.object_dir()] {
        let stat = rustix::fs::stat(path).map_err(|source| io(b"", source))?;
        metadata.push(stat);
    }
    let max_depth = limits.max_depth.min(128);
    let mut path_bytes = limits.max_path_bytes;
    let mut directories = Vec::new();
    let mut previous: &[u8] = b"";
    for entry in entries {
        check(cancel)?;
        validate_path(&entry.path)?;
        if entry.path.iter().filter(|byte| **byte == b'/').count() > max_depth {
            return Err(Error::Limit("directory depth"));
        }
        // Paths sharing a directory are adjacent in index order, so a directory is new exactly
        // when it is not also an ancestor of the previous path.
        for (offset, byte) in entry.path.iter().enumerate() {
            let directory = &entry.path[..offset];
            if *byte == b'/'
                && !(previous.starts_with(directory) && previous.get(offset) == Some(&b'/'))
            {
                charge(&mut path_bytes, offset, "path bytes")?;
                directories.push(directory);
            }
        }
        previous = &entry.path;
    }
    directories.sort_unstable();
    let (policy, rules) = match listing {
        Listing::Raw(policy) => (Policy::Raw(policy), None),
        Listing::Ignoring(rules, files) => (Policy::Ignoring(files), Some(Arc::new(*rules))),
    };
    let ignore_limits = rules.as_ref().map(|rules| rules.limits());
    let ignore_usage = rules.as_ref().map(|rules| rules.usage());
    let scan = Scan {
        format: repo.object_format(),
        tracked: Tracked {
            entries,
            directories,
        },
        metadata,
        index_written,
        policy,
        paths,
        filters,
        max_file_bytes: limits.max_file_bytes,
        max_depth,
        directory_entries: Budget::new(limits.max_directory_entries),
        path_bytes: Budget::new(path_bytes),
        worktree_bytes: Budget::new(limits.max_worktree_bytes),
        ignore_sources: Budget::new(
            ignore_limits
                .map_or(0, |limits| limits.sources)
                .saturating_sub(ignore_usage.map_or(0, |usage| usage.sources)),
        ),
        ignore_bytes: Budget::new(rules.as_ref().map_or(0, |rules| rules.remaining_bytes())),
        ignore_patterns: Budget::new(
            ignore_limits
                .map_or(0, |limits| limits.patterns)
                .saturating_sub(ignore_usage.map_or(0, |usage| usage.patterns)),
        ),
        cancel,
        after_read,
        queue: Mutex::new(Queue {
            tasks: vec![Task {
                open: Open::Root(Arc::clone(&root_fd)),
                path: Vec::new(),
                depth: 0,
                walk: Walk::default(),
                rules,
                listed: false,
            }],
            active: 0,
            failure: None,
        }),
        ready: Condvar::new(),
        stopped: AtomicBool::new(false),
    };
    let found = scan.run(limits.max_threads)?;
    let reopened = open_root(root)?;
    let original = fstat(&*root_fd).map_err(|source| io(b"", source))?;
    let current = fstat(&reopened).map_err(|source| io(b"", source))?;
    same(b"", &original, &current)?;
    let observed = scan.tracked.collect(found.observations);
    let directories = scan.tracked.directory_states(&observed);
    let mut changes = Vec::new();
    for (position, entry) in entries.iter().enumerate() {
        check(cancel)?;
        if entry.stage != index::Stage::Normal || entry.mode == index::Mode::Gitlink {
            continue;
        }
        if let Some(change) = scan.tracked.difference(position, &observed, &directories) {
            changes.push(WorktreeChange {
                path: entry.path.clone(),
                index: value(entry),
                change,
            });
        }
    }
    let Found {
        mut untracked,
        mut listed,
        mut boundaries,
        ..
    } = found;
    untracked.sort_unstable();
    listed.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    boundaries.sort_unstable();
    check(cancel)?;
    Ok(Observation {
        changes,
        untracked,
        listed,
        boundaries,
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
    /// The file's size differs from the cached index size, which Git reports as modified even
    /// when its converted content matches the index.
    Resized(TreeValue),
    Directory,
    Blocked,
}

/// What one worker observed; merged after every worker finishes.
#[derive(Default)]
struct Found {
    /// Observations of tracked paths and their directories, by [`Tracked::slots`].
    observations: Vec<(usize, Observed)>,
    untracked: Vec<Vec<u8>>,
    listed: Vec<UntrackedPath>,
    boundaries: Vec<Vec<u8>>,
}

impl Found {
    fn observe(&mut self, slots: Slots, observed: Observed) {
        for slot in [slots.entry, slots.directory].into_iter().flatten() {
            self.observations.push((slot, observed));
        }
    }

    fn extend(&mut self, other: Self) {
        self.observations.extend(other.observations);
        self.untracked.extend(other.untracked);
        self.listed.extend(other.listed);
        self.boundaries.extend(other.boundaries);
    }
}

struct Scan<'a> {
    format: crate::ObjectFormat,
    tracked: Tracked<'a>,
    metadata: Vec<Stat>,
    /// Seconds word of the index file's mtime; `None` disables trusting cached stat words.
    index_written: Option<u32>,
    policy: Policy,
    /// Paths outside this pathspec are neither read nor listed.
    paths: &'a Pathspec,
    filters: Option<Filters<'a>>,
    max_file_bytes: usize,
    max_depth: usize,
    directory_entries: Budget,
    path_bytes: Budget,
    worktree_bytes: Budget,
    ignore_sources: Budget,
    ignore_bytes: Budget,
    ignore_patterns: Budget,
    cancel: &'a AtomicBool,
    after_read: &'a (dyn Fn(&[u8]) + Sync),
    queue: Mutex<Queue>,
    ready: Condvar,
    /// Set once any worker fails, so the others stop early.
    stopped: AtomicBool,
}

impl Scan<'_> {
    /// Whether the pathspec selects a directory itself, written with its trailing `/`, which is
    /// how Git decides to list an untracked directory or nested repository as one entry.
    fn selects_directory(&self, path: &[u8]) -> bool {
        self.paths.matches_unchecked(&[path, b"/"].concat())
    }

    /// Whether untracked paths are listed at all.
    fn lists_untracked(&self) -> bool {
        match self.policy {
            Policy::Raw(policy) => policy == Untracked::RawFilesWithoutIgnores,
            Policy::Ignoring(files) => files != UntrackedFiles::Omit,
        }
    }

    /// Whether ignore rules exclude this path, given that no ancestor is excluded.
    fn ignored(
        &self,
        rules: Option<&Arc<Ignore>>,
        path: &[u8],
        kind: InputKind,
    ) -> Result<bool, Error> {
        let Some(rules) = rules else {
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

    /// Returns `rules` plus this directory's `.gitignore`. Like Git, a symlink or non-file is
    /// skipped.
    ///
    /// Each directory extends its own copy, so workers never share mutable rules. Source, byte and
    /// pattern counts are also charged to repository-wide budgets, keeping the totals within the
    /// ignore limits as a single sequential walk would.
    fn load_gitignore(
        &self,
        fd: &OwnedFd,
        path: &[u8],
        rules: Arc<Ignore>,
    ) -> Result<Arc<Ignore>, Error> {
        let mut file = path.to_vec();
        if !file.is_empty() {
            file.push(b'/');
        }
        file.extend_from_slice(b".gitignore");
        let Some(stat) = optional_stat(fd, b".gitignore", &file)? else {
            return Ok(rules);
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok(rules);
        }
        let max = self.max_file_bytes.min(self.worktree_bytes.remaining());
        let content = self.read_file(fd, b".gitignore", &file, &stat, max)?;
        self.worktree_bytes
            .charge(content.len(), "worktree bytes")?;
        let limit = |label| Error::Ignore {
            path: file.clone(),
            source: ignore::Error::Limit(label),
        };
        if !self.ignore_sources.take(1) {
            return Err(limit("source count"));
        }
        if !self
            .ignore_bytes
            .take(content.len().saturating_add(path.len()))
        {
            return Err(limit("source bytes"));
        }
        let mut extended = Ignore::clone(&rules);
        let before = extended.usage().patterns;
        extended
            .add(Source::Directory(path), &content)
            .map_err(|source| Error::Ignore {
                path: file.clone(),
                source,
            })?;
        if !self
            .ignore_patterns
            .take(extended.usage().patterns - before)
        {
            return Err(limit("pattern count"));
        }
        Ok(Arc::new(extended))
    }

    /// Opens a child directory found by its parent, walks it and checks it was not replaced.
    #[expect(clippy::too_many_arguments, reason = "one directory's traversal state")]
    fn child_directory(
        &self,
        found: &mut Found,
        parent: &OwnedFd,
        name: &[u8],
        path: &[u8],
        stat: &Stat,
        depth: usize,
        walk: Walk,
        rules: Option<Arc<Ignore>>,
    ) -> Result<Visited, Error> {
        let child_fd = openat(
            parent,
            OsStr::from_bytes(name),
            directory_flags(),
            Mode::empty(),
        )
        .map_err(|source| io(path, source))?;
        let opened = fstat(&child_fd).map_err(|source| io(path, source))?;
        same(path, stat, &opened)?;
        let visited = self.directory(
            found,
            &Arc::new(child_fd),
            &opened,
            path,
            depth,
            walk,
            rules,
        )?;
        let after = statat(parent, OsStr::from_bytes(name), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| io(path, source))?;
        same(path, stat, &after)?;
        Ok(visited)
    }

    /// Walks an open directory whose `fstat` is `before`.
    #[expect(clippy::too_many_arguments, reason = "one directory's traversal state")]
    fn directory(
        &self,
        found: &mut Found,
        fd: &Arc<OwnedFd>,
        before: &Stat,
        path: &[u8],
        depth: usize,
        walk: Walk,
        rules: Option<Arc<Ignore>>,
    ) -> Result<Visited, Error> {
        self.check()?;
        if self
            .metadata
            .iter()
            .any(|metadata| metadata.st_dev == before.st_dev && metadata.st_ino == before.st_ino)
        {
            found.boundaries.push(path.to_vec());
            found.observe(self.tracked.slots(path), Observed::Blocked);
            return Ok(Visited::Metadata);
        }
        let names = self.read_names(fd, path)?;
        // Detect nested repositories before entering any of their children. Merely lstat the
        // marker; never open it. The root marker is expected and ignored below.
        if !path.is_empty() && names.repository_marker(fd, path)? {
            found.boundaries.push(path.to_vec());
            found.observe(self.tracked.slots(path), Observed::Blocked);
            return Ok(Visited::Repository);
        }
        found.observe(self.tracked.slots(path), Observed::Directory);
        // Ignore rules decide only what is listed, so an omitted listing skips them entirely.
        let ignoring = matches!(self.policy, Policy::Ignoring(_)) && self.lists_untracked();
        let rules = match rules {
            Some(rules)
                if !walk.excluded && self.lists_untracked() && names.may_name(b".gitignore") =>
            {
                Some(self.load_gitignore(fd, path, rules)?)
            }
            rules => rules,
        };
        let collapsed = self.policy == Policy::Ignoring(UntrackedFiles::Collapsed);
        for name in names.iter() {
            self.check()?;
            // A collapsed directory needs only one visible untracked path.
            if walk.collapse.is_some_and(|mark| found.listed.len() > mark) {
                break;
            }
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
            // Nothing at or below a path outside the pathspec is reported, so it is not examined.
            let selected = self.paths.matches_unchecked(&child);
            if !selected && self.paths.directory(&child) == Prune::Skip {
                continue;
            }
            let tracked = self.tracked.entry(&child);
            let directory = self.tracked.directory(&child);
            let needed = directory.is_some();
            if tracked.is_none() && !needed && (!self.lists_untracked() || walk.excluded) {
                continue;
            }
            if tracked.is_some_and(|(_, entry)| entry.mode == index::Mode::Gitlink) {
                continue;
            }
            let slot = Slots {
                entry: tracked.map(|(slot, _)| slot),
                directory: directory.map(|directory| self.tracked.entries.len() + directory),
            };
            let stat = statat(&**fd, OsStr::from_bytes(name), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|source| io(&child, source))?;
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind != FileType::Directory {
                if !selected {
                    // A selected tracked path below this one is still obstructed by it.
                    if slot.directory.is_some() {
                        found.observe(slot, Observed::Blocked);
                    }
                    continue;
                }
                self.leaf(
                    found,
                    fd,
                    name,
                    &child,
                    &stat,
                    tracked,
                    slot,
                    rules.as_ref(),
                )?;
                continue;
            }
            if tracked.is_some() && !needed && (!self.lists_untracked() || walk.excluded) {
                found.observe(slot, Observed::Directory);
                continue;
            }
            let mut inner = walk;
            if ignoring {
                let ignored =
                    walk.excluded || self.ignored(rules.as_ref(), &child, InputKind::Directory)?;
                if needed {
                    inner.excluded = ignored;
                } else {
                    // Git's collapsed listing hides a directory at a tracked file's path.
                    if ignored || (tracked.is_some() && collapsed) {
                        found.observe(slot, Observed::Directory);
                        continue;
                    }
                    // Under a pathspec Git collapses a directory only when the spec matches the
                    // directory itself, written with its trailing slash; otherwise it lists the
                    // matching paths inside.
                    if collapsed && inner.collapse.is_none() && self.selects_directory(&child) {
                        inner.collapse = Some(found.listed.len());
                    }
                }
            }
            if depth >= self.max_depth {
                return Err(Error::Limit("directory depth"));
            }
            if inner.collapse.is_some() {
                // A collapsed listing depends on what this worker finds first, so the subtree
                // stays on this worker.
                let visited = self.child_directory(
                    found,
                    fd,
                    name,
                    &child,
                    &stat,
                    depth + 1,
                    inner,
                    rules.clone(),
                )?;
                self.list_directory(found, child, visited, walk, inner);
            } else {
                self.push(Task {
                    open: Open::Child {
                        parent: Arc::clone(fd),
                        name: name.to_vec(),
                        stat,
                    },
                    path: child,
                    depth: depth + 1,
                    walk: inner,
                    rules: rules.clone(),
                    listed: ignoring && !needed,
                });
            }
        }
        let after = fstat(&**fd).map_err(|source| io(path, source))?;
        same(path, before, &after)?;
        Ok(Visited::Directory)
    }

    /// Lists a visible untracked directory after its traversal.
    fn list_directory(
        &self,
        found: &mut Found,
        path: Vec<u8>,
        visited: Visited,
        outer: Walk,
        inner: Walk,
    ) {
        let kind = match visited {
            Visited::Repository if !self.selects_directory(&path) => return,
            Visited::Repository => UntrackedKind::Repository,
            Visited::Metadata => return,
            Visited::Directory => {
                // Only the outermost collapsed directory replaces what was found inside it.
                let Some(mark) = inner.collapse.filter(|_| outer.collapse.is_none()) else {
                    return;
                };
                if found.listed.len() == mark {
                    return;
                }
                found.listed.truncate(mark);
                UntrackedKind::Directory
            }
        };
        found.listed.push(UntrackedPath { path, kind });
    }

    #[expect(clippy::too_many_arguments, reason = "one leaf's observation state")]
    fn leaf(
        &self,
        found: &mut Found,
        fd: &OwnedFd,
        name: &[u8],
        path: &[u8],
        stat: &Stat,
        tracked: Option<(usize, &index::Entry)>,
        slots: Slots,
        rules: Option<&Arc<Ignore>>,
    ) -> Result<(), Error> {
        let kind = FileType::from_raw_mode(stat.st_mode);
        let Some((_, entry)) = tracked else {
            // An untracked leaf can still block a tracked directory of the same name.
            found.observe(slots, Observed::Blocked);
            match self.policy {
                Policy::Raw(Untracked::RawFilesWithoutIgnores) => {
                    found.untracked.push(path.to_vec());
                }
                // Git lists only regular files and symlinks, never sockets or FIFOs.
                Policy::Ignoring(files)
                    if files != UntrackedFiles::Omit
                        && matches!(kind, FileType::RegularFile | FileType::Symlink)
                        && !self.ignored(rules, path, InputKind::File)? =>
                {
                    found.listed.push(UntrackedPath {
                        path: path.to_vec(),
                        kind: UntrackedKind::File,
                    });
                }
                _ => (),
            }
            return Ok(());
        };
        let mode = match kind {
            FileType::RegularFile if stat.st_mode & 0o100 != 0 => EntryMode::Executable,
            FileType::RegularFile => EntryMode::Blob,
            FileType::Symlink => EntryMode::Symlink,
            _ => {
                found.observe(slots, Observed::Blocked);
                return Ok(());
            }
        };
        if self.unchanged(entry, stat, mode) {
            found.observe(slots, Observed::Leaf(value(entry)));
            return Ok(());
        }
        let max = self.max_file_bytes.min(self.worktree_bytes.remaining());
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
        self.worktree_bytes.charge(bytes.len(), "worktree bytes")?;
        (self.after_read)(path);
        self.check()?;
        let after = statat(fd, OsStr::from_bytes(name), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|source| io(path, source))?;
        same(path, stat, &after)?;
        let Some(filters) = self.filters.filter(|_| kind == FileType::RegularFile) else {
            let id = ObjectId::for_blob(self.format, &bytes);
            found.observe(slots, Observed::Leaf(TreeValue { id, mode }));
            return Ok(());
        };
        let id = ObjectId::for_blob(self.format, &filters.convert(path, &bytes, entry.id)?);
        let value = TreeValue { id, mode };
        let cached = entry.stat.size;
        let resized = cached != 0 && cached != stat.st_size as u32;
        found.observe(
            slots,
            if resized {
                Observed::Resized(value)
            } else {
                Observed::Leaf(value)
            },
        );
        Ok(())
    }

    /// Whether the index's cached stat words show this leaf unchanged, so its content need not be
    /// read.
    ///
    /// Every cached word must equal the observed one and the mode must match. An entry is racily
    /// clean, and is read, when its cached mtime or ctime is in or after the second the index was
    /// written: a change within that second could leave every word equal. Git's rule compares
    /// the mtime with nanoseconds; comparing whole seconds of both times rereads a superset of
    /// what Git rereads. A zero cached size is how writers mark an entry for rereading.
    fn unchanged(&self, entry: &index::Entry, stat: &Stat, mode: EntryMode) -> bool {
        let Some(written) = self.index_written else {
            return false;
        };
        // Index words hold the low 32 bits of each value, as Git writes them.
        // Field types differ by platform (Linux nanoseconds are unsigned), so each is truncated
        // directly rather than through a common type.
        let mtime = index::Timestamp {
            seconds: stat.st_mtime as u32,
            nanoseconds: stat.st_mtime_nsec as u32,
        };
        let ctime = index::Timestamp {
            seconds: stat.st_ctime as u32,
            nanoseconds: stat.st_ctime_nsec as u32,
        };
        let cached = &entry.stat;
        entry.stage == index::Stage::Normal
            && value(entry).mode == mode
            && cached.size != 0
            && cached.size == stat.st_size as u32
            && cached.mtime == mtime
            && cached.ctime == ctime
            && cached.device == stat.st_dev as u32
            && cached.inode == stat.st_ino as u32
            && cached.uid == stat.st_uid
            && cached.gid == stat.st_gid
            && cached.mtime.seconds < written
            && cached.ctime.seconds < written
    }

    /// Keep the per-file ceiling distinct from the remaining aggregate read budget.
    fn check_bytes(&self, size: usize) -> Result<(), Error> {
        if size > self.max_file_bytes {
            return Err(Error::Limit("file bytes"));
        }
        if size > self.worktree_bytes.remaining() {
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
        let mut bytes = Vec::with_capacity(size.min(max));
        let mut buffer = [0u8; 64 * 1024];
        loop {
            self.check()?;
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

/// Every name in one directory except `.` and `..`, packed into one buffer.
#[derive(Default)]
struct Names {
    bytes: Vec<u8>,
    ends: Vec<usize>,
}

impl Scan<'_> {
    /// Reads every name in a directory, charging each to the entry and path budgets and checking
    /// cancellation as it is read, so a huge directory cannot outgrow them.
    ///
    /// Reads through a duplicate descriptor, which is cheaper than reopening the directory and
    /// leaves `fd` for the `*at` calls that follow. Those calls do not use the shared offset.
    fn read_names(&self, fd: &OwnedFd, path: &[u8]) -> Result<Names, Error> {
        let duplicate = rustix::io::dup(fd).map_err(|source| io(path, source))?;
        let directory = Dir::new(duplicate).map_err(|source| io(path, source))?;
        let mut names = Names::default();
        for entry in directory {
            self.check()?;
            let entry = entry.map_err(|source| io(path, source))?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            self.directory_entries.charge(1, "directory entries")?;
            let length = path
                .len()
                .checked_add(name.len())
                .and_then(|n| n.checked_add(usize::from(!path.is_empty())))
                .ok_or(Error::Limit("path bytes"))?;
            self.path_bytes.charge(length, "path bytes")?;
            names.bytes.extend_from_slice(name);
            names.ends.push(names.bytes.len());
        }
        Ok(names)
    }
}

impl Names {
    fn iter(&self) -> impl Iterator<Item = &[u8]> {
        let starts = std::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(&self.ends)
            .map(|(start, end)| &self.bytes[start..*end])
    }

    /// Whether `lstat` of the ASCII name `target` could succeed here. A case-insensitive
    /// filesystem resolves any ASCII case variant, and non-ASCII names are not ruled out.
    fn may_name(&self, target: &[u8]) -> bool {
        self.iter()
            .any(|name| !name.is_ascii() || name.eq_ignore_ascii_case(target))
    }

    /// [`repository_marker`], skipping each `lstat` that no listed name could satisfy.
    fn repository_marker(&self, fd: &OwnedFd, path: &[u8]) -> Result<bool, Error> {
        let present = |name: &[u8]| -> Result<bool, Error> {
            Ok(self.may_name(name) && optional_stat(fd, name, path)?.is_some())
        };
        Ok(present(b".git")? || (present(b"HEAD")? && present(b"objects")? && present(b"refs")?))
    }
}

/// Index entries and the directories that contain them, looked up by path.
///
/// Every tracked path and directory has a slot: entries by their position in the index (the first
/// stage for a conflicted path), then directories after them.
pub(crate) fn validate_path(path: &[u8]) -> Result<(), Error> {
    if path.is_empty()
        || path.contains(&0)
        || path.split(|byte| *byte == b'/').any(|part| {
            part.is_empty() || part == b"." || part == b".." || part.eq_ignore_ascii_case(b".git")
        })
    {
        return Err(unsupported(path, "unsafe path"));
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

mod parallel;
#[cfg(test)]
mod tests;
mod tracked;
