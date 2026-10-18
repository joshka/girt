use super::*;

pub(super) fn run(
    mut scan: Scan<'_>,
    mut limits: Limits,
    cancel: &AtomicBool,
) -> Result<Report, Error> {
    if scan.paths.is_some_and(|paths| !paths.is_all()) {
        return Err(Error::Unsupported {
            path: Vec::new(),
            reason: "path-limited status cache refresh",
        });
    }
    let repo = scan.repo;
    let mut edit = repo.edit_index(limits.index)?;
    if edit
        .index()
        .extensions()
        .iter()
        .any(|extension| extension.signature() == *b"link")
    {
        return Err(Error::Unsupported {
            path: Vec::new(),
            reason: "split-index status cache refresh",
        });
    }
    let cache_enabled = repo.config().boolean("core", None, "untrackedcache") != Ok(Some(false))
        && (repo.config().boolean("core", None, "untrackedcache") == Ok(Some(true))
            || edit
                .index()
                .extensions()
                .iter()
                .any(|extension| extension.signature() == *b"UNTR"));
    let capture = if cache_enabled {
        if scan.untracked != UntrackedFiles::Collapsed
            || crate::status::workflow::supported::git_names(repo)?
                != crate::status::worktree::GitNames::default()
        {
            return Err(Error::Unsupported {
                path: Vec::new(),
                reason: "untracked cache refresh requires collapsed listing and exact byte names",
            });
        }
        let capture = std::sync::Arc::new(crate::status::untracked::Capture::default());
        scan.capture = Some(capture.clone());
        Some(capture)
    } else {
        None
    };
    let excludes = scan.ignore.untracked_excludes(repo.object_format());
    let exclude_stats = if capture.is_some() {
        exclude_stats(scan.ignore, &mut limits)?
    } else {
        Default::default()
    };
    let started = edit
        .locked_at()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| u32::try_from(time.as_secs()).unwrap_or(u32::MAX));
    let checkpoint = if let Some(monitor) = scan.monitor.take() {
        if scan.repo.worktree() != Some(monitor.worktree()) {
            return Err(Error::Fsmonitor(Box::new(fsmonitor::Error::Unsupported(
                "monitor belongs to another working tree",
            ))));
        }
        let old = edit.index().fsmonitor_cache(limits.index).ok().flatten();
        let changes = monitor
            .query_with(
                old.as_ref().map(crate::index::FsmonitorCache::token),
                Default::default(),
                cancel,
            )
            .map_err(|error| {
                if matches!(error, fsmonitor::Error::Cancelled) {
                    Error::Cancelled
                } else {
                    Error::Fsmonitor(Box::new(error))
                }
            })?;
        Some(changes.token().clone())
    } else {
        None
    };
    let report = scan.run_with(limits, cancel)?;
    crate::status::types::check(cancel)?;
    let mut written = false;
    if let Some(capture) = capture {
        let cache = capture
            .encode(
                repo,
                excludes,
                exclude_stats,
                &report.untracked,
                started,
                limits.index,
            )
            .map_err(|source| crate::index::StorageError::Format {
                path: repo.git_dir().join("index"),
                source,
            })?;
        edit.set_untracked_cache(&cache)
            .map_err(|source| crate::index::StorageError::Format {
                path: repo.git_dir().join("index"),
                source,
            })?;
        written = true;
    } else if repo.config().boolean("core", None, "untrackedcache") == Ok(Some(false)) {
        let present = edit
            .index()
            .extensions()
            .iter()
            .any(|extension| extension.signature() == *b"UNTR");
        edit.set_untracked_cache_enabled(false).map_err(|source| {
            crate::index::StorageError::Format {
                path: repo.git_dir().join("index"),
                source,
            }
        })?;
        written |= present;
    }
    if let Some(token) = checkpoint {
        let changed: std::collections::HashSet<_> = report
            .unstaged
            .iter()
            .map(|change| change.path.as_slice())
            .collect();
        let dirty = edit
            .index()
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| {
                (entry.stage != crate::index::Stage::Normal
                    || entry.intent_to_add
                    || entry.mode == crate::index::Mode::Gitlink
                    || changed.contains(entry.path.as_slice()))
                .then_some(position)
            })
            .collect();
        let cache = crate::index::FsmonitorCache::new(
            token,
            dirty,
            edit.index().entries().len(),
            limits.index,
        )
        .map_err(|source| crate::index::StorageError::Format {
            path: repo.git_dir().join("index"),
            source,
        })?;
        edit.set_fsmonitor_cache(Some(&cache)).map_err(|source| {
            crate::index::StorageError::Format {
                path: repo.git_dir().join("index"),
                source,
            }
        })?;
        crate::status::types::check(cancel)?;
        written = true;
    }
    crate::status::types::check(cancel)?;
    if written {
        edit.commit()?;
    } else {
        edit.abort()?;
    }
    Ok(report)
}

/// Pins and revalidates loaded base ignore bytes before attaching their stat hints to UNTR.
fn exclude_stats(
    rules: &crate::ignore::WorktreeRules,
    limits: &mut Limits,
) -> Result<[crate::index::Stat; 2], Error> {
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let mut stats = [crate::index::Stat::default(); 2];
    for position in 0.. {
        let Some(source) = rules.source(position) else {
            break;
        };
        let [info, global] = &mut stats;
        let target = match source.kind() {
            crate::ignore::SourceKind::Info => info,
            crate::ignore::SourceKind::Global => global,
            crate::ignore::SourceKind::Directory(_) => continue,
        };
        let path = source.path().as_os_str().as_bytes().to_vec();
        let io = |source| Error::Io {
            path: path.clone(),
            source,
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed())
            .open(source.path())
            .map_err(io)?;
        let before = file.metadata().map_err(io)?;
        if !before.is_file() {
            return Err(Error::Changed(path));
        }
        let size = source.content().len();
        if size > limits.max_file_bytes || size > limits.max_worktree_bytes {
            return Err(Error::Limit("base exclude bytes"));
        }
        let mut content = Vec::new();
        (&file)
            .take((size as u64).saturating_add(1))
            .read_to_end(&mut content)
            .map_err(io)?;
        let after = file.metadata().map_err(io)?;
        let stat = |metadata: &std::fs::Metadata| crate::index::Stat {
            ctime: crate::index::Timestamp {
                seconds: metadata.ctime() as u32,
                nanoseconds: metadata.ctime_nsec() as u32,
            },
            mtime: crate::index::Timestamp {
                seconds: metadata.mtime() as u32,
                nanoseconds: metadata.mtime_nsec() as u32,
            },
            device: metadata.dev() as u32,
            inode: metadata.ino() as u32,
            uid: metadata.uid(),
            gid: metadata.gid(),
            size: metadata.size() as u32,
        };
        if content != source.content()
            || stat(&before) != stat(&after)
            || before.mode() != after.mode()
        {
            return Err(Error::Changed(path));
        }
        limits.max_worktree_bytes -= size;
        *target = stat(&before);
    }
    Ok(stats)
}
