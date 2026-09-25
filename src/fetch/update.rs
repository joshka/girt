use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use super::{FetchError, FetchLimits, FetchUpdate, FetchUpdateKind, check_cancelled};
use crate::refs::RefName;
use crate::{HistoryLimits, ObjectId, ObjectKind, Objects, PackLimits, Tag};

/// Bounds for reopening installed objects and checking remote-tracking update ancestry.
///
/// History limits apply separately to each changed commit-valued destination. Total work can be
/// that budget times the number of mappings (bounded by transfer `max_wants`). Tag reads apply the
/// history's per-read limits. Reference enumeration/transactions retain their existing unbounded
/// metadata contracts. These are work/input bounds, not wall-clock or process-heap limits.
#[derive(Clone, Copy, Debug)]
pub struct FetchUpdateLimits {
    /// Destination pack snapshot and installation dependency-check bounds.
    pub snapshot: PackLimits,
    /// Bounds for rereading the installed selected-tip graph through the destination object store.
    /// Uses `max_wants`, `max_known_objects`, `max_known_bytes`, `max_known_edges`, `max_haves`
    /// and `known_read` through [`super::KnownHistory::new`]; other fields do not apply to
    /// this phase. This detects corrupt loose objects shadowing valid installed pack objects.
    pub verification: FetchLimits,
    /// Commit/edge/per-read bounds for each ancestry decision.
    pub history: HistoryLimits,
    /// Maximum tag objects followed per old/new tip (default 32).
    pub max_tag_depth: usize,
}
impl Default for FetchUpdateLimits {
    fn default() -> Self {
        Self {
            snapshot: PackLimits::default(),
            verification: FetchLimits::default(),
            history: HistoryLimits::default(),
            max_tag_depth: 32,
        }
    }
}

/// An installed transfer cannot be safely published under the selected update rules.
#[derive(Debug, thiserror::Error)]
pub enum FetchUpdateError {
    /// Failed reading, parsing, peeling, cancellation or a tag-depth limit.
    #[error(transparent)]
    Object(#[from] FetchError),
    /// Commit ancestry could not be established within the explicit history bounds.
    #[error(transparent)]
    History(#[from] crate::HistoryError),
    /// Both tips peel to commits but the replacement rewinds/diverges and is not both forced
    /// and explicitly authorized for this destination name.
    #[error("non-fast-forward fetch requires force intent and authorization: {0:?}")]
    NonFastForward(RefName),
}

pub(super) fn validate(
    updates: &mut [FetchUpdate],
    objects: &Objects,
    authorized: &BTreeSet<RefName>,
    limits: FetchUpdateLimits,
    cancel: &AtomicBool,
) -> Result<(), FetchUpdateError> {
    for update in updates {
        check_cancelled(cancel)?;
        if update.kind != FetchUpdateKind::Replace {
            continue;
        }
        let old = update.previous.expect("replacement has old value");
        let new = update.mapping.source.as_ref().expect("fetch source").id;
        let old_commit = peel_commit(objects, old, limits, cancel)?;
        let new_commit = peel_commit(objects, new, limits, cancel)?;
        let (Some(old), Some(new)) = (old_commit, new_commit) else {
            continue;
        };
        // Existing history traversal is synchronous and bounded, with no cancellation inside one
        // query. Check immediately on either side; never start publication after cancellation.
        let fast_forward = objects.is_ancestor(old, new, limits.history)?;
        check_cancelled(cancel)?;
        let name = update
            .mapping
            .destination
            .as_ref()
            .expect("replacement destination");
        update.kind = commit_update_kind(
            name,
            fast_forward,
            update.mapping.force,
            authorized.contains(name),
        )?;
    }
    Ok(())
}

fn commit_update_kind(
    name: &RefName,
    fast_forward: bool,
    force: bool,
    authorized: bool,
) -> Result<FetchUpdateKind, FetchUpdateError> {
    if fast_forward {
        Ok(FetchUpdateKind::FastForward)
    } else if force && authorized {
        Ok(FetchUpdateKind::ForcedTracking)
    } else {
        Err(FetchUpdateError::NonFastForward(name.clone()))
    }
}

fn peel_commit(
    objects: &Objects,
    mut id: ObjectId,
    limits: FetchUpdateLimits,
    cancel: &AtomicBool,
) -> Result<Option<ObjectId>, FetchError> {
    let mut expected = None;
    for depth in 0..=limits.max_tag_depth {
        check_cancelled(cancel)?;
        let object = objects
            .read(id, limits.history.read)?
            .ok_or(FetchError::Missing(id))?;
        if expected.is_some_and(|kind| object.kind() != kind) {
            return Err(FetchError::Kind(id));
        }
        match object.kind() {
            ObjectKind::Commit => return Ok(Some(id)),
            ObjectKind::Tag => {
                if depth == limits.max_tag_depth {
                    return Err(FetchError::Limit("update tag depth"));
                }
                let tag = Tag::parse(crate::ObjectFormat::Sha1, object.data())
                    .map_err(|source| FetchError::Tag { id, source })?;
                id = tag.fields().target;
                expected = Some(tag.fields().target_kind);
            }
            _ => return Ok(None),
        }
    }
    unreachable!("bounded peeling returns or rejects the last tag")
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::fast_forward(true, false, false, FetchUpdateKind::FastForward)]
    #[case::forced(false, true, true, FetchUpdateKind::ForcedTracking)]
    fn commit_decisions(
        #[case] ff: bool,
        #[case] force: bool,
        #[case] auth: bool,
        #[case] expected: FetchUpdateKind,
    ) {
        let name = RefName::new("refs/remotes/origin/main").unwrap();
        assert_eq!(
            commit_update_kind(&name, ff, force, auth).unwrap(),
            expected
        );
    }

    #[rstest]
    #[case::neither(false, false)]
    #[case::syntax_only(true, false)]
    #[case::authorization_only(false, true)]
    fn reject_unauthorized_rewind(#[case] force: bool, #[case] auth: bool) {
        let name = RefName::new("refs/remotes/origin/main").unwrap();
        assert!(matches!(
            commit_update_kind(&name, false, force, auth),
            Err(FetchUpdateError::NonFastForward(_))
        ));
    }
}
