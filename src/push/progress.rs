//! Local push preparation, separate from upload and receiver messages.

/// Completed local work while constructing a prepared push.
///
/// Observers run synchronously and must return promptly. Counts do not describe upload, receiver
/// acceptance or reference publication. Set the preparation cancellation flag to stop at the next
/// cooperative check; one storage read, hash or compression call cannot be interrupted.
///
/// # Examples
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::push::{PreparationProgress, PreparedPush, PushCommand, PushLimits};
/// use girt::refs::RefName;
/// # let (_directory, _, repo, _, _) = include!("../doctest/remote.rs");
/// # let commit = include!("../doctest/commit.rs");
/// let first = commit(&repo, &[], "First commit")?;
/// let command = PushCommand::new(RefName::new("refs/heads/main")?, None, first);
/// let objects = repo.objects_with(PackLimits::default())?;
///
/// let mut events = Vec::new();
/// let cancel = AtomicBool::new(false);
/// PreparedPush::new_with_progress(
///     &objects,
///     vec![command],
///     &[],
///     PushLimits::default(),
///     &cancel,
///     |event| events.push(event),
/// )?;
/// assert!(events.contains(&PreparationProgress::Packing { objects: (2, 2) }));
/// assert_eq!(events.last(), Some(&PreparationProgress::Complete));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PreparationProgress {
    /// Objects examined in storage so far, including known receiver history read to find the
    /// exclusion boundary. Selected packed blobs use framing instead of decoded payloads. The total
    /// remains unknown during traversal.
    Reading {
        /// Completed source objects; begins at zero and never decreases.
        objects: u64,
    },
    /// Successfully encoded outgoing entries after receiver-history exclusion.
    /// Finishing these counts does not establish a finished pack/index pair.
    Packing {
        /// `(done, total)` for the selected unique objects.
        objects: (u64, u64),
    },
    /// The prepared temporary pack and final cancellation check succeeded. Last notification;
    /// setting cancellation here cannot undo preparation. No commands have been sent or refs
    /// published.
    Complete,
}
