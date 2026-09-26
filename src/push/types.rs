use crate::refs::RefName;
use crate::{ObjectId, PackCompression, PackWriteLimits, ReadLimits};

/// Policy for replacing an existing destination. Server restrictions still apply.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ForcePolicy {
    /// Branch updates require the old commit to be an ancestor of the new commit. Existing tags
    /// can only retain the same ID. Other namespaces use only the exact old-value condition.
    /// Creation is allowed; branches must point directly to commits.
    #[default]
    FastForwardOnly,
    /// Explicitly permit non-fast-forward branch changes or tag replacement, still conditional
    /// on the exact expected old value. This never bypasses server hooks or configuration.
    Allow,
}

/// One full destination name and an exact compare-and-swap expectation.
#[derive(Debug, Clone)]
pub struct PushCommand {
    /// Validated full destination name under `refs/`; `HEAD` is not a push destination.
    pub name: RefName,
    /// `None` requires absence. `Some` requires this exact nonzero value, even with force enabled.
    pub expected: Option<ObjectId>,
    /// Desired tip. The null ID in the repository's format deletes an existing ref. Tags and
    /// non-branch namespaces may point to any supported object kind.
    pub new: ObjectId,
    /// Explicit replacement policy; use the default to protect existing history and tags.
    pub force: ForcePolicy,
}
impl PushCommand {
    /// Returns whether this command deletes its destination.
    pub fn deletes(&self) -> bool {
        self.new.is_null()
    }
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
    /// Total encoded command and push-option bytes, including flushes (default 16 MiB).
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
    /// its bound still applies. Full selected count/payload bounds apply before exclusion.
    pub pack: PackWriteLimits,
    /// Explicit pack compression policy; ordinary entries by default. Delta bases stay internal
    /// after receiver-history exclusion and need no receive-pack capability negotiation.
    pub compression: PackCompression,
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
            compression: PackCompression::Ordinary,
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
    /// Whether any bytes of this command may have reached the peer, or native publication
    /// reached this command. `false` proves this command was not attempted by this push. A true
    /// value without an acknowledgement is uncertain and requires remote inspection.
    pub attempted: bool,
    /// Native reference and reflog effects, when publication was attempted. Wire servers do not
    /// expose this detail. A published reference can accompany a failed reflog append.
    pub effects: Option<crate::refs::RefEditOutcome>,
    /// Actual destination(s) reported by `report-status-v2` for a successful proc-receive
    /// command. Empty for ordinary updates or v1 servers. Each entry is server evidence, not a
    /// local reference mutation.
    pub rewrites: Vec<RefRewrite>,
    /// Whether all v2 rewrite options for this acknowledgement were terminated by the next
    /// status or report flush. False after an interrupted `ok` packet: the command was
    /// acknowledged, but its actual rewritten destination may still be unknown.
    pub rewrite_complete: bool,
}

/// One server-reported replacement for a successful pseudo-reference command.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct RefRewrite {
    /// Actual destination. `None` means the submitted name was used.
    pub name: Option<RefName>,
    /// Actual previous ID, when reported by the server.
    pub old: Option<ObjectId>,
    /// Actual new ID, when reported by the server.
    pub new: Option<ObjectId>,
    /// Whether the server marked the update as forced.
    pub forced: bool,
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
    /// Bounded channel-2 progress frames when sideband reporting was requested. Bytes are
    /// untrusted and need not be UTF-8. Frames received before failure remain in the report.
    pub progress: Vec<Vec<u8>>,
    /// Native receive warnings that did not reject a reference.
    pub warnings: Vec<Vec<u8>>,
}
impl PushReport {
    pub(super) fn pending(commands: &[PushCommand]) -> Self {
        Self {
            unpack: None,
            progress: vec![],
            warnings: vec![],
            refs: commands
                .iter()
                .cloned()
                .map(|command| RefStatus {
                    command,
                    status: None,
                    attempted: false,
                    effects: None,
                    rewrites: vec![],
                    rewrite_complete: false,
                })
                .collect(),
        }
    }

    pub(super) fn mark_attempted(&mut self, request: &[u8], written: usize) {
        let mut offset = 0usize;
        for reference in &mut self.refs {
            if written > offset {
                reference.attempted = true;
            }
            let header = &request[offset..offset + 4];
            let length =
                usize::from_str_radix(std::str::from_utf8(header).expect("encoded packet"), 16)
                    .expect("encoded packet length");
            offset += length;
        }
    }

    /// Whether every requested update was acknowledged and unpacking succeeded. An empty push
    /// is successful. Inspect individual results even when this returns false.
    pub fn all_succeeded(&self) -> bool {
        (self.refs.is_empty() || self.unpack == Some(Status::Ok))
            && self
                .refs
                .iter()
                .all(|r| r.status == Some(Status::Ok) && r.rewrite_complete)
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
    /// Native local destination could not be opened.
    #[error("local destination: {0}")]
    Destination(#[source] Box<crate::OpenError>),
    /// Native object installation failed before reference publication.
    #[error("local object installation: {0}")]
    Install(#[source] Box<crate::fetch::FetchError>),
    /// Native reference publication failed after mutation may have begun.
    #[error("local reference publication: {0}")]
    Reference(#[source] Box<crate::refs::ReferenceError>),
    /// Explicit reflog identity is invalid for a new record.
    #[error("invalid push reflog identity")]
    Identity,
    /// A supplied identity uses a different object format from the source repository.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// Sanitized OpenSSH transport or service failure.
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    #[error("{0}")]
    Ssh(#[source] crate::transport::ssh::SshError),
    /// Sanitized smart HTTP exchange failure.
    #[cfg(feature = "http")]
    #[error("{0}")]
    Http(#[source] crate::transport::http::HttpError),
    /// I/O failure; protocol interruption is returned without retrying.
    #[error("push I/O: {0}")]
    Io(#[source] std::io::Error),
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
    /// The owned transport reached its caller-supplied deadline.
    #[error("transport deadline expired")]
    Deadline,
    /// A root used for exclusion is no longer advertised. No commands were sent.
    #[error("receiver history root is not advertised: {0}")]
    KnowledgeChanged(ObjectId),
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
    #[error("push object {id}: {source}")]
    Read {
        /// Reachable object being read.
        id: ObjectId,
        /// Concrete storage failure.
        #[source]
        source: crate::ObjectReadError,
    },
    /// Pack construction failed before sending commands.
    #[error("push pack: {0}")]
    Pack(#[from] crate::PackWriteError),
    /// Reachable commit payload invalid or unsupported.
    #[error("push commit {id}: {source}")]
    Commit {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::CommitError,
    },
    /// Reachable tree payload invalid or unsupported.
    #[error("push tree {id}: {source}")]
    Tree {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::TreeError,
    },
    /// Reachable tag payload invalid or unsupported.
    #[error("push tag {id}: {source}")]
    Tag {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::TagError,
    },
    /// Local receive-pack exited unsuccessfully, even if some results were acknowledged.
    #[error("local receive-pack exited unsuccessfully: {0}")]
    Process(std::process::ExitStatus),
}
impl PushFailure {
    pub(super) fn destination(error: crate::OpenError) -> Self {
        Self::Destination(Box::new(error))
    }

    pub(super) fn install(error: crate::fetch::FetchError) -> Self {
        Self::Install(Box::new(error))
    }

    pub(super) fn reference(error: crate::refs::ReferenceError) -> Self {
        Self::Reference(Box::new(error))
    }
}
impl From<crate::packet::Error> for PushFailure {
    fn from(error: crate::packet::Error) -> Self {
        match error {
            crate::packet::Error::Io(e) => Self::from(e),
            crate::packet::Error::Protocol(e) => Self::Protocol(e),
            crate::packet::Error::Limit(e) => Self::Limit(e),
            crate::packet::Error::Cancelled => Self::Cancelled,
            crate::packet::Error::Remote(e) => Self::Remote(e),
        }
    }
}

impl From<std::io::Error> for PushFailure {
    fn from(error: std::io::Error) -> Self {
        match crate::transport::interruption(&error) {
            Some(crate::transport::Interruption::Cancelled) => Self::Cancelled,
            Some(crate::transport::Interruption::Deadline) => Self::Deadline,
            None => Self::Io(error),
        }
    }
}

#[cfg(feature = "http")]
impl From<crate::transport::http::HttpError> for PushFailure {
    fn from(error: crate::transport::http::HttpError) -> Self {
        match error {
            crate::transport::http::HttpError::Cancelled => Self::Cancelled,
            crate::transport::http::HttpError::Deadline => Self::Deadline,
            error => Self::Http(error),
        }
    }
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
impl From<crate::transport::ssh::SshError> for PushFailure {
    fn from(error: crate::transport::ssh::SshError) -> Self {
        match error {
            crate::transport::ssh::SshError::Cancelled => Self::Cancelled,
            crate::transport::ssh::SshError::Deadline => Self::Deadline,
            error => Self::Ssh(error),
        }
    }
}

impl From<crate::edges::Error> for PushFailure {
    fn from(error: crate::edges::Error) -> Self {
        match error {
            crate::edges::Error::Commit { id, source } => Self::Commit { id, source },
            crate::edges::Error::Tree { id, source } => Self::Tree { id, source },
            crate::edges::Error::Tag { id, source } => Self::Tag { id, source },
        }
    }
}
