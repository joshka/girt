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
    let mut name = RefName::new("HEAD").expect("valid HEAD");
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
