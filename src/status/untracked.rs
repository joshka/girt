//! Admission of Git-written directory-cache claims; filesystem freshness is checked by the walk.
use std::collections::BTreeMap;
mod write;

use super::worktree::{GitNames, Listing};
use super::{Limits, UntrackedCacheFallback, UntrackedCacheUse, UntrackedFiles};
use crate::{ObjectId, Repository, index};

#[derive(Default)]
pub(super) struct Capture {
    pub(super) directories: std::sync::Mutex<BTreeMap<Vec<u8>, Record>>,
}
pub(super) struct Record {
    pub(super) stat: index::Stat,
    pub(super) exclude: Option<ObjectId>,
    pub(super) untracked: Vec<Vec<u8>>,
    pub(super) check_only: bool,
    pub(super) valid: bool,
}

pub(super) struct Cache {
    pub(super) directories: BTreeMap<Vec<u8>, Directory>,
}
pub(super) struct Directory {
    pub(super) cached: index::UntrackedDirectory,
    pub(super) children: Vec<Vec<u8>>,
}

pub(super) fn load(
    repo: &Repository,
    index: Option<&index::Index>,
    listing: &Listing,
    paths: &crate::pathspec::Pathspec,
    limits: Limits,
    cancel: &std::sync::atomic::AtomicBool,
) -> (Option<Cache>, UntrackedCacheUse) {
    let fallback = |reason| (None, UntrackedCacheUse::Fallback(reason));
    let Listing::Ignoring {
        files,
        names,
        excludes,
        capture,
        ..
    } = listing
    else {
        return (None, UntrackedCacheUse::Disabled);
    };

    if capture.is_some() {
        return (None, UntrackedCacheUse::Disabled);
    }

    if repo.config().boolean("core", None, "untrackedcache") == Ok(Some(false)) {
        return (None, UntrackedCacheUse::Disabled);
    }

    if *files != UntrackedFiles::Collapsed || !paths.is_all() {
        return fallback(UntrackedCacheFallback::ListingPolicy);
    }

    if *names != GitNames::default() {
        return fallback(UntrackedCacheFallback::NamePolicy);
    }

    let Some(index) = index else {
        return (None, UntrackedCacheUse::NotPresent);
    };

    if index.shared_index_id().is_some() {
        return fallback(UntrackedCacheFallback::SplitIndex);
    }

    let cache = match index.untracked_cache(limits.index) {
        Ok(Some(cache)) => cache,
        Ok(None) => return (None, UntrackedCacheUse::NotPresent),
        Err(_) => return fallback(UntrackedCacheFallback::Malformed),
    };

    if cache.flags() != 6 || cache.exclude_name() != b".gitignore" {
        return fallback(UntrackedCacheFallback::ListingPolicy);
    }

    let mut identity = b"Location ".to_vec();
    use std::os::unix::ffi::OsStrExt as _;
    let Some(root) = repo.worktree() else {
        return fallback(UntrackedCacheFallback::Environment);
    };
    let Ok(identity_root) = root.canonicalize() else {
        return fallback(UntrackedCacheFallback::Environment);
    };
    identity.extend_from_slice(identity_root.as_os_str().as_bytes());
    identity.extend_from_slice(if cfg!(target_os = "macos") {
        b", system Darwin\0"
    } else {
        b", system Linux\0"
    });

    if cache.ident() != identity {
        return fallback(UntrackedCacheFallback::Environment);
    }

    if excludes.as_ref() != Some(cache.excludes()) {
        return fallback(UntrackedCacheFallback::IgnoreRules);
    }

    match directories(cache.directories(), limits, cancel) {
        Some(directories) => (
            Some(Cache { directories }),
            UntrackedCacheUse::Reused { directories: 0 },
        ),
        None => fallback(UntrackedCacheFallback::Malformed),
    }
}

fn directories(
    nodes: &[index::UntrackedDirectory],
    limits: Limits,
    cancel: &std::sync::atomic::AtomicBool,
) -> Option<BTreeMap<Vec<u8>, Directory>> {
    let mut output: BTreeMap<Vec<u8>, Directory> = BTreeMap::new();

    let mut parents: Vec<(Vec<u8>, usize)> = Vec::new();
    let mut remaining_bytes = limits.max_path_bytes;

    for (position, node) in nodes.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        while parents.last().is_some_and(|(_, remaining)| *remaining == 0) {
            parents.pop();
        }

        let path = if position == 0 {
            if !node.name().is_empty() {
                return None;
            }
            Vec::new()
        } else {
            let (parent, remaining) = parents.last_mut()?;
            *remaining = remaining.checked_sub(1)?;

            component(node.name())?;

            let length = parent
                .len()
                .checked_add(node.name().len())?
                .checked_add(usize::from(!parent.is_empty()))?;
            if length > limits.index.max_path_bytes {
                return None;
            }

            remaining_bytes = remaining_bytes.checked_sub(length)?;

            let mut path = parent.clone();
            if !path.is_empty() {
                path.push(b'/');
            }

            path.extend_from_slice(node.name());
            output
                .get_mut(parent.as_slice())?
                .children
                .push(node.name().to_vec());
            path
        };

        if parents.len() > limits.max_depth.min(128) {
            return None;
        }

        for name in node.untracked() {
            component(name.strip_suffix(b"/").unwrap_or(name))?;
        }

        if output
            .insert(
                path.clone(),
                Directory {
                    cached: node.clone(),
                    children: Vec::new(),
                },
            )
            .is_some()
        {
            return None;
        }

        parents.push((path, node.children()));
    }

    Some(output)
}
fn component(name: &[u8]) -> Option<()> {
    if name.is_empty()
        || name.contains(&b'/')
        || name == b"."
        || name == b".."
        || name.eq_ignore_ascii_case(b".git")
    {
        None
    } else {
        Some(())
    }
}

/// Git appends a newline to a nonempty ignore buffer, including when one is already present.
pub(super) fn ignore_id(format: crate::ObjectFormat, bytes: &[u8]) -> ObjectId {
    let mut bytes = bytes.to_vec();
    if !bytes.is_empty() {
        bytes.push(b'\n');
    }
    ObjectId::for_blob(format, &bytes)
}
