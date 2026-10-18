use std::sync::atomic::{AtomicBool, Ordering};

use super::{Operation, Options, Replay, Status};
use crate::{Commit, ObjectId, Objects, Signature};

/// Bounds and single-step policy for object-only rebase.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct RebaseOptions {
    /// Policy applied independently to each selected source commit.
    pub replay: Options,
    /// Maximum input sequence length, checked before reading any commit (default 10,000).
    pub max_commits: usize,
}
impl Default for RebaseOptions {
    fn default() -> Self {
        Self {
            replay: Options::default(),
            max_commits: 10_000,
        }
    }
}

/// One completed input and its resulting identity.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RebasedCommit {
    /// Original input identity.
    pub source: ObjectId,
    /// Proposed destination identity, or the unchanged tip when this input was skipped.
    pub destination: ObjectId,
    /// Full replay result, including generated objects and optional proposed commit.
    pub replay: Replay,
}

/// Object-only rebase progress, stopping before the first unresolved input.
///
/// Install completed steps' blobs, trees and commits in sequence order before publishing `tip`.
/// A stopped result additionally owns its candidate objects and conflicts; those do not authorize
/// publishing the provisional candidate as resolved. Nothing has been installed by this result.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Rebase {
    /// Original explicit destination.
    pub onto: ObjectId,
    /// Last completed commit identity, equal to `onto` when no input completed.
    pub tip: ObjectId,
    /// Completed steps in input order, including explicitly skipped inputs.
    pub completed: Vec<RebasedCommit>,
    /// First conflict or stop-on-empty result. Its source has not completed.
    pub stopped: Option<Replay>,
}

/// Rebase failed without publishing any computed object or reference.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RebaseError {
    /// Input length exceeded the explicit bound.
    #[error("rebase commit count exceeds configured bound")]
    Limit,
    /// Cancellation observed between replay steps.
    #[error("rebase cancelled")]
    Cancelled,
    /// A source or initial destination could not be read.
    #[error("reading rebase commit")]
    Read(#[from] crate::ReadCommitError),
    /// Single-step computation failed; completed in-memory objects are discarded.
    #[error("replaying rebase input")]
    Replay(#[from] super::Error),
}

impl Objects {
    /// Replays explicit ordered commits onto a destination entirely in memory.
    ///
    /// The caller selects and orders the range; no ancestry walk, patch-ID filtering, fast-forward
    /// reuse or merge-topology reconstruction occurs. Each input creates a new single-parent commit
    /// or follows its explicit empty policy. Source authors are retained and the supplied committer
    /// is used for every new commit. Merge inputs require the replay mainline option.
    ///
    /// Generated trees and blobs remain visible to subsequent steps through an in-memory overlay.
    /// The first conflict or stop-on-empty outcome returns completed progress and the unresolved
    /// replay. Skipped inputs map to the current tip. Errors or cancellation discard all progress;
    /// no object, state file, working file, index or reference has changed.
    ///
    /// # Errors
    ///
    /// Returns [`RebaseError`] for input bounds, cancellation, unreadable inputs or replay failure.
    pub fn rebase(
        &self,
        commits: &[ObjectId],
        onto: ObjectId,
        committer: Signature,
        options: RebaseOptions,
        cancel: &AtomicBool,
    ) -> Result<Rebase, RebaseError> {
        run_rebase(self, commits, onto, committer, options, cancel, None)
    }

    /// Replays a sequence with the same caller-owned content driver at every step.
    ///
    /// Uses [`Self::rebase`]'s explicit range, metadata, progress and publication contracts. Driver
    /// snapshots and side effects remain caller-owned; no attributes or processes run implicitly.
    ///
    /// # Errors
    ///
    /// Returns [`RebaseError`], retaining path-driver failures in the replay error source chain.
    pub fn rebase_with_driver(
        &self,
        commits: &[ObjectId],
        onto: ObjectId,
        committer: Signature,
        options: RebaseOptions,
        cancel: &AtomicBool,
        driver: &mut dyn crate::merge::TreeDriver,
    ) -> Result<Rebase, RebaseError> {
        run_rebase(
            self,
            commits,
            onto,
            committer,
            options,
            cancel,
            Some(driver),
        )
    }
}

fn run_rebase<'a, 'd>(
    objects: &'a Objects,
    commits: &[ObjectId],
    onto: ObjectId,
    committer: Signature,
    options: RebaseOptions,
    cancel: &'a AtomicBool,
    mut driver: Option<&'a mut (dyn crate::merge::TreeDriver + 'd)>,
) -> Result<Rebase, RebaseError> {
    if commits.len() > options.max_commits {
        return Err(RebaseError::Limit);
    }
    let mut current: Commit = objects.read_commit(onto)?;
    let mut memory = crate::merge::ReplayTrees::default();
    let mut result = Rebase {
        onto,
        tip: onto,
        completed: Vec::new(),
        stopped: None,
    };
    for &source in commits {
        if cancel.load(Ordering::Relaxed) {
            return Err(RebaseError::Cancelled);
        }
        let original = objects.read_commit(source)?;
        let operation = Operation {
            source,
            original: &original,
            onto: &current,
            committer: committer.clone(),
            options: options.replay,
            reverse: false,
        };
        let replay = operation.compute(objects, cancel, &mut memory, driver.as_deref_mut())?;
        if matches!(replay.status, Status::Conflicted | Status::Empty(_)) {
            result.stopped = Some(replay);
            return Ok(result);
        }
        if let Some(commit) = &replay.commit {
            current = commit.clone();
        }
        result.tip = current.id();
        result.completed.push(RebasedCommit {
            source,
            destination: result.tip,
            replay,
        });
    }
    Ok(result)
}
