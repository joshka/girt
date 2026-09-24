use crate::refs::RefName;
use crate::{ObjectId, PackWriteLimits, ReadLimits};

/// Policy for replacing an existing destination. Server restrictions still apply.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ForcePolicy {
    /// Branch updates require the old commit to be an ancestor of the new commit. Existing tags
    /// can only retain the same ID. Creation is allowed; branches must point directly to commits.
    #[default]
    FastForwardOnly,
    /// Explicitly permit non-fast-forward branch changes or tag replacement, still conditional
    /// on the exact expected old value. This never bypasses server hooks or configuration.
    Allow,
}

/// One full destination name and an exact compare-and-swap expectation.
#[derive(Debug, Clone)]
pub struct PushCommand {
    /// Validated name under `refs/heads/` or `refs/tags/`; other namespaces are rejected.
    pub name: RefName,
    /// `None` requires absence. `Some` requires this nonzero SHA-1 value, even with force enabled.
    pub expected: Option<ObjectId>,
    /// Nonzero desired tip. Deletion is not supported. Tags may point to any supported object
    /// kind.
    pub new: ObjectId,
    /// Explicit replacement policy; use the default to protect existing history and tags.
    pub force: ForcePolicy,
}

/// Input, work and output bounds for preparation and a single receive-pack session.
///
/// Retained memory is proportional to selected payloads, pack bytes, object/edge counts and
/// protocol bytes, plus the caller's object snapshot. Structured parsing temporarily copies
/// payloads. Per-read decoding work is bounded separately and may recur for each selected object.
/// These are not hard heap or wall-clock limits; allocator overhead and server memory are excluded.
#[derive(Debug, Clone, Copy)]
pub struct PushLimits {
    /// Advertisement bytes including pkt-line framing (default 4 MiB).
    pub max_advertisement_bytes: usize,
    /// Advertised reference and `.have` entries (default 100,000).
    pub max_refs: usize,
    /// Explicit command count (default 100,000).
    pub max_commands: usize,
    /// Total encoded command bytes, including flush (default 16 MiB).
    pub max_command_bytes: usize,
    /// Status bytes including framing (default 4 MiB).
    pub max_status_bytes: usize,
    /// Reachable edge occurrences, including duplicates (default 4 million).
    pub max_edges: usize,
    /// Cumulative commit visits and parent edge occurrences across fast-forward proofs
    /// (default 4 million), including duplicates.
    pub max_ancestry_steps: usize,
    /// Per-object storage decoding bounds. Payload reads are additionally capped by remaining
    /// aggregate pack input bytes so preparation cannot retain more than that payload budget.
    pub read: ReadLimits,
    /// Selected count/payload and generated artifact bounds. The index is generated into a sink;
    /// its bound still applies. No pack is omitted on repeated or incremental updates.
    pub pack: PackWriteLimits,
}
impl Default for PushLimits {
    fn default() -> Self {
        Self {
            max_advertisement_bytes: 4 * 1024 * 1024,
            max_refs: 100_000,
            max_commands: 100_000,
            max_command_bytes: 16 * 1024 * 1024,
            max_status_bytes: 4 * 1024 * 1024,
            max_edges: 4_000_000,
            max_ancestry_steps: 4_000_000,
            read: ReadLimits::default(),
            pack: PackWriteLimits::default(),
        }
    }
}

/// Server acknowledgement, preserving rejection text without assuming UTF-8.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Status {
    /// The server reported success.
    Ok,
    /// The server reported failure with this byte-preserving explanation.
    Rejected(Vec<u8>),
}

/// Evidence for one submitted command, in caller command order.
#[derive(Debug, Clone)]
pub struct RefStatus {
    /// The submitted destination, old/new IDs and force policy.
    pub command: PushCommand,
    /// `None` means no valid acknowledgement was received. It does not mean rejection.
    pub status: Option<Status>,
}

/// Server evidence, including individual results for a non-atomic multi-ref push.
///
/// A successful `send` returns a complete report even when unpacking or every reference failed.
/// An uncertain error retains whatever valid status prefix was received. An `ok` is the server's
/// acknowledgement, not a promise of durability or that another writer has not since moved the ref.
#[derive(Debug, Clone)]
pub struct PushReport {
    /// Pack acceptance, or `None` when no unpack result was received (also for an empty push).
    pub unpack: Option<Status>,
    /// Per-ref results in request order. Empty for an empty push.
    pub refs: Vec<RefStatus>,
}
impl PushReport {
    pub(super) fn pending(commands: &[PushCommand]) -> Self {
        Self {
            unpack: None,
            refs: commands
                .iter()
                .cloned()
                .map(|command| RefStatus {
                    command,
                    status: None,
                })
                .collect(),
        }
    }

    /// Whether every requested update was acknowledged and unpacking succeeded. An empty push
    /// is successful. Inspect individual results even when this returns false.
    pub fn all_succeeded(&self) -> bool {
        (self.refs.is_empty() || self.unpack == Some(Status::Ok))
            && self.refs.iter().all(|r| r.status == Some(Status::Ok))
    }
}

/// Session failure classified by whether commands might have reached the server.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// No update commands were attempted; this operation did not mutate destination refs.
    #[error("push not sent: {0}")]
    NotSent(#[source] PushFailure),
    /// Command transmission began. Some or all updates may have happened; do not blindly retry.
    /// Retained acknowledgements are evidence, while missing results require remote inspection.
    #[error("push outcome uncertain: {cause}")]
    Uncertain {
        /// Transport, protocol, cancellation, resource or process failure.
        #[source]
        cause: PushFailure,
        /// Valid status prefix received before failure; missing entries remain unknown.
        report: Box<PushReport>,
    },
}

/// Preparation or session failure cause. Remote per-ref rejections are [`Status`] values instead.
#[derive(Debug, thiserror::Error)]
pub enum PushFailure {
    /// I/O failure; protocol interruption is returned without retrying.
    #[error("push I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed or unexpected receive-pack framing or state.
    #[error("invalid receive-pack response: {0}")]
    Protocol(&'static str),
    /// Unsupported protocol, format, command namespace or required capability.
    #[error("unsupported push feature: {0}")]
    Unsupported(&'static str),
    /// Peer ERR packet, not a per-reference rejection report.
    #[error("receive-pack error: {0:?}")]
    Remote(Vec<u8>),
    /// Resource bound exhausted.
    #[error("push limit exceeded: {0}")]
    Limit(&'static str),
    /// Cancellation observed between operations.
    #[error("push cancelled")]
    Cancelled,
    /// Advertisement disagreed with the explicit expectation. No commands were sent.
    #[error("stale expectation for {name:?}: expected {expected:?}, advertised {actual:?}")]
    Stale {
        /// Destination name.
        name: RefName,
        /// Caller expectation, with `None` meaning absent.
        expected: Option<ObjectId>,
        /// Advertised value, with `None` meaning unadvertised.
        actual: Option<ObjectId>,
    },
    /// Duplicate destination, zero ID, or other invalid command.
    #[error("invalid push command: {0}")]
    Command(&'static str),
    /// Non-fast-forward branch update or tag replacement without explicit force.
    #[error("replacement requires explicit force for {0:?}")]
    WouldForce(RefName),
    /// Missing selected tip or reachable object.
    #[error("missing reachable object {0}")]
    Missing(ObjectId),
    /// Branch tip or reachable edge has the wrong object type.
    #[error("wrong reachable object kind for {0}")]
    Kind(ObjectId),
    /// Bounded object storage read failed.
    #[error("push object read: {0}")]
    Read(#[from] crate::ObjectReadError),
    /// Pack construction failed before sending commands.
    #[error("push pack: {0}")]
    Pack(#[from] crate::PackWriteError),
    /// Reachable commit payload invalid or unsupported.
    #[error("push commit: {0}")]
    Commit(#[from] crate::CommitError),
    /// Reachable tree payload invalid or unsupported.
    #[error("push tree: {0}")]
    Tree(#[from] crate::TreeError),
    /// Reachable tag payload invalid or unsupported.
    #[error("push tag: {0}")]
    Tag(#[from] crate::TagError),
    /// Local receive-pack exited unsuccessfully, even if some results were acknowledged.
    #[error("local receive-pack exited unsuccessfully: {0}")]
    Process(std::process::ExitStatus),
}
impl From<crate::packet::Error> for PushFailure {
    fn from(error: crate::packet::Error) -> Self {
        match error {
            crate::packet::Error::Io(e) => Self::Io(e),
            crate::packet::Error::Protocol(e) => Self::Protocol(e),
            crate::packet::Error::Limit(e) => Self::Limit(e),
            crate::packet::Error::Cancelled => Self::Cancelled,
            crate::packet::Error::Remote(e) => Self::Remote(e),
        }
    }
}
