//! Local push preparation, separate from upload and receiver messages.

/// Completed local work while constructing a prepared push.
///
/// Observers run synchronously and must return promptly. Counts do not describe upload, receiver
/// acceptance or reference publication. Set the preparation cancellation flag to stop at the next
/// cooperative check; one storage read, hash or compression call cannot be interrupted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparationProgress {
    /// Objects read from storage so far, including known receiver history read to find the
    /// exclusion boundary. The total remains unknown during traversal.
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
    /// Prepared buffers and the final cancellation check succeeded. Last notification; setting
    /// cancellation here cannot undo preparation. No commands have been sent or refs published.
    Complete,
}
