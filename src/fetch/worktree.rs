use std::fs;

use super::FetchPlanError;
use crate::Repository;
use crate::refs::{RefName, ReferenceError, Target};

// Restrict HEAD chains instead of approximating branch-in-use detection. With the caller excluding
// checkout/HEAD/registration changes, no supported destination can be part of a worktree's HEAD.
pub(super) fn check(repository: &Repository) -> Result<(), FetchPlanError> {
    let main = Repository::open(repository.common_dir())?;
    check_head(&main)?;
    let entries = match fs::read_dir(repository.common_dir().join("worktrees")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            return Err(ReferenceError::Unsupported("non-directory worktree registration").into());
        }
        let worktree = Repository::open(entry.path())?;
        check_head(&worktree)?;
    }
    Ok(())
}

fn check_head(repository: &Repository) -> Result<(), FetchPlanError> {
    let refs = repository.references()?;
    let mut name = RefName::HEAD;
    for _ in 0..=32 {
        match refs.read(&name)? {
            Some(Target::Symbolic(next)) if next.as_bytes().starts_with(b"refs/heads/") => {
                name = next
            }
            Some(Target::Symbolic(next)) => return Err(FetchPlanError::Head(next)),
            _ => return Ok(()),
        }
    }
    Err(ReferenceError::Depth(32).into())
}

// Inspect at most one registration. Even malformed entries require admission, so never open them.
pub(super) fn admit_controlled(
    repository: &Repository,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), FetchPlanError> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(FetchPlanError::Cancelled);
    }
    if repository.git_dir() != repository.common_dir() {
        return Err(FetchPlanError::LinkedWorktrees);
    }
    match fs::read_dir(repository.common_dir().join("worktrees")) {
        Ok(mut entries) => {
            if let Some(entry) = entries.next() {
                entry?;
                return Err(FetchPlanError::LinkedWorktrees);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn check_head_controlled(
    repository: &Repository,
    listed: &[crate::refs::Reference],
    limits: super::workflow::FetchPrepareLimits,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), FetchPlanError> {
    let refs = repository
        .references()?
        .with_reftable_limits(limits.reftable);
    let mut target = refs.head_controlled(limits.max_head_bytes, cancel)?;
    for _ in 0..=32 {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(FetchPlanError::Cancelled);
        }
        match target {
            Some(Target::Symbolic(next)) if next.as_bytes().starts_with(b"refs/heads/") => {
                target = listed
                    .binary_search_by(|reference| reference.name.cmp(&next))
                    .ok()
                    .and_then(|index| listed.get(index))
                    .map(|reference| reference.target.clone());
            }
            Some(Target::Symbolic(next)) => return Err(FetchPlanError::Head(next)),
            _ => return Ok(()),
        }
    }
    Err(ReferenceError::Depth(32).into())
}
