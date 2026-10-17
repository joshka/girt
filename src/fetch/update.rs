use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use super::{FetchError, FetchLimits, FetchUpdate, FetchUpdateKind, check_cancelled};
use crate::refs::RefName;
use crate::{HistoryLimits, ObjectId, ObjectKind, PackLimits, PeelLimits};

/// Bounds for reopening installed objects and checking remote-tracking update ancestry.
///
/// These apply in [`FetchReady::finish`](super::FetchReady::finish), after the transfer: reopening
/// the destination's object store, verifying the installed tips, and walking history to decide
/// whether each update fast-forwards. Pair `default()` with
/// [`FetchLimits::default`](super::FetchLimits::default) for untrusted remotes, and `trusted()`
/// with [`FetchLimits::trusted`](super::FetchLimits::trusted) otherwise; see [Limits and
/// trust](crate::transport#limits-and-trust).
///
/// # Examples
///
/// ```
/// use girt::fetch::{FetchLimits, FetchUpdateLimits};
///
/// // A large repository the user owns, fetching from a remote they configured.
/// let transfer = FetchLimits::trusted();
/// let update = FetchUpdateLimits::trusted();
/// assert_eq!(update.max_tag_depth, 10_000);
/// # let _ = transfer;
/// ```
///
/// History limits apply separately to each changed commit-valued destination. Total work can be
/// that budget times the number of mappings (bounded by transfer `max_wants`). Tag reads apply the
/// history's per-read limits. Reference enumeration/transactions retain their existing unbounded
/// metadata contracts. These are work/input bounds, not wall-clock or process-heap limits.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct FetchUpdateLimits {
    /// Destination pack snapshot and installation dependency-check bounds.
    pub snapshot: PackLimits,
    /// Bounds for verifying the installed selected-tip graph through destination lookup.
    /// Uses `max_wants`, `max_known_objects`, `max_known_bytes`, `max_known_edges`, `max_haves`
    /// and `known_read` through [`super::KnownHistory::new`]; other fields do not apply to
    /// this phase. Byte-bound receive evidence avoids repeated decoding, while preserving these
    /// counts and per-read bounds. Other destination representations use ordinary checked reads.
    pub verification: FetchLimits,
    /// Commit/edge/per-read bounds for each ancestry decision.
    /// The verified breadth-first walk stops at the old tip. Commits discovered before that match
    /// and followed parent occurrences count; the old tip's parents are not followed. Negative
    /// decisions exhaust the new tip's reachable history up to shallow boundaries, without walking
    /// the old tip's history. Both tips still undergo checked peeling before the ancestry query.
    pub history: HistoryLimits,
    /// Maximum tag objects followed per old/new tip (default 32).
    pub max_tag_depth: usize,
}

impl FetchUpdateLimits {
    /// Sets [`Self::snapshot`].
    #[must_use]
    pub fn with_snapshot(mut self, snapshot: PackLimits) -> Self {
        self.snapshot = snapshot;
        self
    }

    /// Sets [`Self::verification`].
    #[must_use]
    pub fn with_verification(mut self, verification: FetchLimits) -> Self {
        self.verification = verification;
        self
    }

    /// Sets [`Self::history`].
    #[must_use]
    pub fn with_history(mut self, history: HistoryLimits) -> Self {
        self.history = history;
        self
    }

    /// Sets [`Self::max_tag_depth`].
    #[must_use]
    pub fn with_max_tag_depth(mut self, max_tag_depth: usize) -> Self {
        self.max_tag_depth = max_tag_depth;
        self
    }
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

impl FetchUpdateLimits {
    /// Returns limits for a repository the caller trusts as Git trusts the user's own.
    ///
    /// Git bounds none of these values. The defaults suit untrusted input; these are large enough
    /// that only available memory bounds the work, while keeping headroom for internal arithmetic.
    pub const fn trusted() -> Self {
        Self {
            snapshot: PackLimits::trusted(),
            verification: FetchLimits::trusted(),
            history: HistoryLimits::trusted(),
            max_tag_depth: 10_000,
        }
    }
}

/// An installed transfer cannot be safely published under the selected update rules.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchUpdateError {
    /// Failed reading, parsing, peeling, cancellation or a tag-depth limit.
    #[error(transparent)]
    Object(#[from] FetchError),
    /// Annotated-tag resolution failed before reference publication.
    #[error(transparent)]
    Peel(#[from] crate::PeelError),
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
    verified: &super::verified::InstalledObjects<'_>,
    authorized: &BTreeSet<RefName>,
    limits: FetchUpdateLimits,
    cancel: &AtomicBool,
) -> Result<(), FetchUpdateError> {
    let objects = verified.store();
    for update in updates {
        check_cancelled(cancel)?;
        if update.kind != FetchUpdateKind::Replace {
            continue;
        }
        let old = update.previous.expect("replacement has old value");
        let new = update.mapping.source.as_ref().expect("fetch source").id;
        let old_commit = peel_commit(verified, old, limits, cancel)?;
        let new_commit = peel_commit(verified, new, limits, cancel)?;
        let (Some(old), Some(new)) = (old_commit, new_commit) else {
            continue;
        };
        let fast_forward =
            objects.is_ancestor_verified(old, new, limits.history, cancel, |id, read| {
                let Some(object) = verified
                    .get(id, read, cancel)
                    .map_err(|source| crate::HistoryError::Read { id, source })?
                else {
                    return Ok(None);
                };
                if object.kind != ObjectKind::Commit {
                    return Err(crate::HistoryError::NotCommit(id));
                }
                let parents = object
                    .edges()
                    .expect("InstalledObjects::get requires parsed edges")
                    .iter()
                    .filter_map(|&(id, kind)| (kind == ObjectKind::Commit).then_some(id))
                    .collect();
                Ok(Some(parents))
            })?;
        check_cancelled(cancel)?;
        let name = update
            .mapping
            .destination
            .as_ref()
            .expect("replacement destination");
        update.kind = commit_update_kind(
            name,
            fast_forward,
            update.mapping.force == crate::remote::ForceIntent::Requested,
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
    verified: &super::verified::InstalledObjects<'_>,
    id: ObjectId,
    limits: FetchUpdateLimits,
    cancel: &AtomicBool,
) -> Result<Option<ObjectId>, FetchUpdateError> {
    let mut read = limits.history.read;
    read.max_object_bytes = read
        .max_object_bytes
        .min(limits.verification.max_known_bytes);
    if let Some(object) = verified
        .get(id, read, cancel)
        .map_err(|source| FetchError::LocalRead { id, source })?
        && object.kind != ObjectKind::Tag
    {
        return Ok((object.kind == ObjectKind::Commit).then_some(id));
    }
    let result = verified.store().peel(
        id,
        PeelLimits {
            max_tags: limits.max_tag_depth,
            max_bytes: limits.verification.max_known_bytes,
            read: limits.history.read,
        },
        cancel,
    )?;
    Ok((result.kind == ObjectKind::Commit).then_some(result.target))
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
