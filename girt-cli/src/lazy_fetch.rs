//! Fetching objects a partial clone lacks on demand, as Git's lazy fetch does.
//!
//! The library never fetches while reading. Commands that read file content run the operation,
//! and when it fails with [`MissingObjectsError`] in a partial clone, fetch the listed objects in
//! one batch from the promisor remotes and run it again. Setting `GIT_NO_LAZY_FETCH` to a true
//! value turns this off, so the typed error is reported instead.

use std::collections::HashSet;

use color_eyre::eyre::{Report, Result, WrapErr as _, eyre};
use girt::{MissingObjectsError, ObjectId, Repository};

use crate::Context;

/// Runs `operation`, fetching the objects it reports missing and running it again until it
/// succeeds, fails for another reason, or asks again for objects already fetched.
///
/// Operations that list every missing object up front, such as checkout and patches, run at
/// most twice. Blame reports one missing version at a time, so it fetches once per version.
pub fn retry<T, E>(
    context: &Context,
    repository: &Repository,
    mut operation: impl FnMut() -> Result<T, E>,
) -> Result<T>
where
    E: Into<Report>,
{
    let mut fetched = HashSet::new();
    loop {
        let error = match operation() {
            Ok(value) => return Ok(value),
            Err(error) => error.into(),
        };
        let Some(missing) = missing_objects(&error) else {
            return Err(error);
        };
        // Objects fetched for an earlier attempt are not requested again; when nothing is new,
        // the fetch did not help and the error stands.
        let wanted: Vec<ObjectId> = missing.ids().filter(|id| !fetched.contains(id)).collect();
        if wanted.is_empty() || disabled(context)? {
            return Err(error);
        }
        let mut callbacks = crate::progress::Progress::new(crate::progress::Visibility::Hidden);
        girt::workflow::fetch_promised(repository, &wanted, &context.environment, &mut callbacks)
            .wrap_err_with(|| {
            format!(
                "could not fetch {} missing objects from promisor remote",
                wanted.len()
            )
        })?;
        fetched.extend(wanted);
    }
}

/// The missing objects that stopped an operation, if that is why it failed.
fn missing_objects(error: &Report) -> Option<&MissingObjectsError> {
    error.chain().find_map(|cause| cause.downcast_ref())
}

/// Whether `GIT_NO_LAZY_FETCH` is set to a true value. Like Git, an empty value is false and a
/// value that isn't a boolean is an error.
pub fn disabled(context: &Context) -> Result<bool> {
    let Some(value) = context.environment.get("GIT_NO_LAZY_FETCH") else {
        return Ok(false);
    };
    let bytes = value.as_encoded_bytes();
    girt::config::parse_boolean(bytes).ok_or_else(|| {
        eyre!(
            "bad boolean environment value '{}' for 'GIT_NO_LAZY_FETCH'",
            String::from_utf8_lossy(bytes)
        )
    })
}
