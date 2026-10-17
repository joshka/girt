use crate::refs::RefName;
use crate::{ObjectId, PackCompression, PackWriteLimits, ReadLimits};

/// Policy for replacing an existing destination. Server restrictions still apply.
///
/// Force is checked twice: [`PreparedPush::new`](super::PreparedPush::new) refuses a
/// non-fast-forward branch update under [`FastForwardOnly`](Self::FastForwardOnly) before anything
/// is sent, and the receiver applies its own policy, such as `receive.denyNonFastForwards`. Even
/// [`Allow`](Self::Allow) keeps the exact expected-value condition, so it is a
/// `--force-with-lease`, never a blind `--force`.
///
/// # Examples
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::push::{ForcePolicy, PreparedPush, PushCommand, PushFailure, PushLimits};
/// use girt::refs::RefName;
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
/// # let commit = include!("../doctest/commit.rs");
/// # girt::workflow::fetch(&repo, "origin", &girt::workflow::FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]), &girt::workflow::Environment::default(), &mut girt::workflow::NoCallbacks)?;
/// // `repo` has fetched `main`; `rewritten` does not descend from it.
/// let rewritten = commit(&repo, &[], "Rewritten history")?;
/// let objects = repo.objects(PackLimits::default())?;
/// let cancel = AtomicBool::new(false);
/// let name = RefName::new("refs/heads/main")?;
///
/// let rewind = PushCommand::new(name.clone(), Some(main), rewritten);
/// let refused = PreparedPush::new(&objects, vec![rewind.clone()], &[], PushLimits::default(), &cancel);
/// assert!(matches!(refused, Err(PushFailure::WouldForce(_))));
///
/// let forced = rewind.with_force(ForcePolicy::Allow);
/// assert!(PreparedPush::new(&objects, vec![forced], &[], PushLimits::default(), &cancel).is_ok());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
#[non_exhaustive]
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
///
/// A command says "if `name` is `expected`, make it `new`". Use `expected: None` to create a
/// reference that must not exist yet, and the null ID as `new` to delete one. Get `expected` from
/// the value you last saw on the remote, such as your remote-tracking reference; never guess it.
///
/// The condition guards changes. If the reference already has the value `new`, nothing needs to
/// change and the receiver reports `Ok` without comparing `expected`, so an `Ok` does not prove
/// that the expectation matched.
///
/// # Examples
///
/// ```
/// use girt::push::{ForcePolicy, PushCommand};
/// use girt::refs::RefName;
/// use girt::{ObjectFormat, ObjectId, ObjectKind};
///
/// let old = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"old");
/// let new = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"new");
///
/// // Create `topic`; rejected if it already exists with a different value.
/// let create = PushCommand::new(RefName::new("refs/heads/topic")?, None, new);
/// // Fast-forward `main` from `old` to `new`.
/// let update = PushCommand::new(RefName::new("refs/heads/main")?, Some(old), new);
/// // Delete `stale`, but only if it is still at `old`.
/// let delete = PushCommand::new(
///     RefName::new("refs/heads/stale")?,
///     Some(old),
///     ObjectId::null(ObjectFormat::Sha1),
/// )
/// .with_force(ForcePolicy::Allow);
/// assert!(delete.deletes() && !create.deletes() && !update.deletes());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// Creates a conditional update protected by [`ForcePolicy::FastForwardOnly`].
    ///
    /// An `expected` value of `None` requires the reference to be absent; `Some` requires
    /// that exact nonzero object ID, even when force is subsequently enabled.
    pub fn new(name: RefName, expected: Option<ObjectId>, new: ObjectId) -> Self {
        Self {
            name,
            expected,
            new,
            force: ForcePolicy::default(),
        }
    }

    /// Sets [`Self::name`].
    #[must_use]
    pub fn with_name(mut self, name: RefName) -> Self {
        self.name = name;
        self
    }

    /// Sets [`Self::expected`].
    #[must_use]
    pub fn with_expected(mut self, expected: Option<ObjectId>) -> Self {
        self.expected = expected;
        self
    }

    /// Sets [`Self::new`].
    #[must_use]
    pub fn with_new(mut self, new: ObjectId) -> Self {
        self.new = new;
        self
    }

    /// Sets [`Self::force`].
    #[must_use]
    pub fn with_force(mut self, force: ForcePolicy) -> Self {
        self.force = force;
        self
    }
}
impl PushCommand {
    /// Returns whether this command deletes its destination.
    pub fn deletes(&self) -> bool {
        self.new.is_null()
    }
}

/// Input, work and output bounds for preparation and a single receive-pack session.
///
/// Push limits bound work on both sides: preparing a push walks and decodes the history it sends
/// and builds the pack, and sending parses the receiver's advertisement and status. Preparation
/// copies eligible compressed entries after a CRC check rather than rehashing them, so the
/// receiver, not the pusher, establishes those objects' identities; see
/// [`PreparedPush::new`](super::PreparedPush::new) for the source storage assumptions. Use
/// `default()` when either side is untrusted and `trusted()` otherwise, as
/// [`crate::workflow::push`] does. See [Limits and trust](crate::transport#limits-and-trust).
///
/// # Examples
///
/// ```
/// use girt::PackCompression;
/// use girt::push::PushLimits;
///
/// // Accept larger pushes than the default, without delta search for faster preparation.
/// let limits = PushLimits::trusted().with_compression(PackCompression::Ordinary);
/// assert_eq!(limits.max_commands, PushLimits::trusted().max_commands);
/// ```
///
/// Packs are spooled to temporary files. Retained memory includes selected structured payloads,
/// object/edge metadata, protocol bytes and the caller's object snapshot. Packed blob payloads
/// are not retained by selection; writing holds one reconstructed object and, for fresh delta
/// search, up to `window * max_object_bytes` internal candidate bytes plus one external candidate.
/// Structured parsing copies payloads.
/// Per-read decoding work is bounded separately and may recur for each selected object.
/// These are not hard heap or wall-clock limits; allocator overhead and server memory are excluded.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PushLimits {
    /// Advertisement bytes including pkt-line framing (default 4 MiB).
    pub max_advertisement_bytes: usize,
    /// Advertised reference and `.have` entries (default 100,000).
    pub max_refs: usize,
    /// Explicit command count (default 100,000).
    pub max_commands: usize,
    /// Total encoded command and push-option bytes, including flushes (default 16 MiB).
    pub max_command_bytes: usize,
    /// Status bytes including framing (default 4 MiB). Native receive hooks share this bound
    /// for their combined retained stdout/stderr across the whole operation. Excess native hook
    /// output is discarded while draining continues; truncation does not abort hooks.
    pub max_status_bytes: usize,
    /// Edge occurrences from objects selected for sending, including duplicates (default 4
    /// million). Reading known receiver history has a separate allowance of the same size.
    pub max_edges: usize,
    /// Cumulative commit visits and parent edge occurrences across fast-forward proofs
    /// (default 4 million), including duplicates. Both the new and old tips' ancestry count.
    pub max_ancestry_steps: usize,
    /// Per-object storage decoding bounds. Payload reads are additionally capped by remaining
    /// aggregate pack input bytes so preparation cannot retain more than that payload budget.
    pub read: ReadLimits,
    /// Selected count/payload and generated artifact bounds. The index is generated into a sink;
    /// its bound still applies. Count/payload bounds cover every object read as a candidate for
    /// sending, including commits later found in receiver history.
    pub pack: PackWriteLimits,
    /// Compression policy for fresh and reconstructed objects; bounded delta search by default.
    /// Blob and tree candidates may come from verified advertised-root history; their
    /// reconstructed values count against `pack.max_input_bytes`. Insufficient candidate
    /// budget falls back to whole objects. A corrupt offered candidate refuses preparation even
    /// if another candidate could produce a valid delta; candidate integrity is checked strictly.
    /// Existing compressed entries and eligible deltas are reused under either policy.
    /// Transfer deltas use REF_DELTA, requiring no optional OFS capability. Search and copied
    /// chains obey `read.max_delta_depth` and the delta policy's depth bound when enabled.
    /// Offering receiver candidates additionally caps output depth at one so internal bases
    /// always resolve before receiver-assisted external lookup.
    pub compression: PackCompression,
    /// Bounds for opening a native local destination repository's object store, to check its
    /// history and install the pushed pack (default
    /// [`PackLimits::default`](crate::PackLimits::default), which admits 64 MiB of pack index).
    ///
    /// Git-scale destinations exceed the default: a Linux kernel clone has a 300 MB index. Use
    /// [`PackLimits::trusted`](crate::PackLimits::trusted), as [`Self::trusted`] does, for
    /// repositories the caller trusts. Network pushes open no destination store and ignore this.
    pub destination_packs: crate::PackLimits,
}

impl PushLimits {
    /// Sets [`Self::max_advertisement_bytes`].
    #[must_use]
    pub fn with_max_advertisement_bytes(mut self, max_advertisement_bytes: usize) -> Self {
        self.max_advertisement_bytes = max_advertisement_bytes;
        self
    }

    /// Sets [`Self::max_refs`].
    #[must_use]
    pub fn with_max_refs(mut self, max_refs: usize) -> Self {
        self.max_refs = max_refs;
        self
    }

    /// Sets [`Self::max_commands`].
    #[must_use]
    pub fn with_max_commands(mut self, max_commands: usize) -> Self {
        self.max_commands = max_commands;
        self
    }

    /// Sets [`Self::max_command_bytes`].
    #[must_use]
    pub fn with_max_command_bytes(mut self, max_command_bytes: usize) -> Self {
        self.max_command_bytes = max_command_bytes;
        self
    }

    /// Sets [`Self::max_status_bytes`].
    #[must_use]
    pub fn with_max_status_bytes(mut self, max_status_bytes: usize) -> Self {
        self.max_status_bytes = max_status_bytes;
        self
    }

    /// Sets [`Self::max_edges`].
    #[must_use]
    pub fn with_max_edges(mut self, max_edges: usize) -> Self {
        self.max_edges = max_edges;
        self
    }

    /// Sets [`Self::max_ancestry_steps`].
    #[must_use]
    pub fn with_max_ancestry_steps(mut self, max_ancestry_steps: usize) -> Self {
        self.max_ancestry_steps = max_ancestry_steps;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }

    /// Sets [`Self::pack`].
    #[must_use]
    pub fn with_pack(mut self, pack: PackWriteLimits) -> Self {
        self.pack = pack;
        self
    }

    /// Sets [`Self::compression`].
    #[must_use]
    pub fn with_compression(mut self, compression: PackCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Sets [`Self::destination_packs`].
    #[must_use]
    pub fn with_destination_packs(mut self, destination_packs: crate::PackLimits) -> Self {
        self.destination_packs = destination_packs;
        self
    }
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
            compression: PackCompression::Delta(crate::DeltaOptions::DEFAULT),
            destination_packs: crate::PackLimits::default(),
        }
    }
}

impl PushLimits {
    /// Returns limits for a transfer the caller trusts as Git trusts the user's own remotes.
    ///
    /// Git bounds none of these values. The defaults suit untrusted input; these are large enough
    /// that only available memory bounds the work, while keeping headroom for internal arithmetic.
    ///
    /// The prepared pack is held in a temporary file before sending.
    pub const fn trusted() -> Self {
        const UNBOUNDED: usize = usize::MAX / 4;
        Self {
            max_advertisement_bytes: UNBOUNDED,
            max_refs: UNBOUNDED,
            max_commands: UNBOUNDED,
            max_command_bytes: UNBOUNDED,
            max_status_bytes: UNBOUNDED,
            max_edges: UNBOUNDED,
            max_ancestry_steps: UNBOUNDED,
            read: ReadLimits::trusted(),
            pack: PackWriteLimits::trusted(),
            compression: PackCompression::Delta(crate::DeltaOptions::DEFAULT),
            destination_packs: crate::PackLimits::trusted(),
        }
    }
}

/// Server acknowledgement, preserving rejection text without assuming UTF-8.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Status {
    /// The server reported success.
    Ok,
    /// The server reported failure with this byte-preserving explanation.
    Rejected(Vec<u8>),
}

/// Provenance of a rejected reference update.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum RejectionOrigin {
    /// The native destination's locked reference did not match the command's expected old value.
    ExpectedValue,
    /// A receiver rejected the command for policy, storage, or another reason. Wire status
    /// reports use this variant even when their reason text mentions a stale value: the client
    /// cannot independently prove the receiver's exact comparison.
    Receiver,
}

/// Evidence for one submitted command, in caller command order.
///
/// Read `status` first: `Some(Status::Ok)` is an acknowledgement, `Some(Status::Rejected(_))` a
/// refusal with the server's reason, and `None` an unknown outcome, never a refusal. When it is
/// `None` and `attempted` is true, the update may have happened; inspect the remote.
///
/// # Examples
///
/// See [`PushReport`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RefStatus {
    /// The submitted destination, old/new IDs and force policy.
    pub command: PushCommand,
    /// `None` means no valid acknowledgement was received. It does not mean rejection.
    pub status: Option<Status>,
    /// Rejection provenance, present only when `status` is [`Status::Rejected`].
    pub rejection_origin: Option<RejectionOrigin>,
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
#[non_exhaustive]
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
/// # Examples
///
/// One command succeeds and one is rejected because the remote value changed:
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::push::{
///     ForcePolicy, LocalPushRequest, PreparedPush, PushCommand, PushLimits, RejectionOrigin,
///     Status, send_local,
/// };
/// use girt::refs::RefName;
/// use girt::transport::TransportControl;
/// use girt::{ObjectFormat, ObjectKind};
/// # let (_directory, upstream, repo, main, _) = include!("../doctest/remote.rs");
/// # let commit = include!("../doctest/commit.rs");
/// # girt::workflow::fetch(&repo, "origin", &girt::workflow::FetchOptions::new(["+refs/heads/*:refs/remotes/origin/*"]), &girt::workflow::Environment::default(), &mut girt::workflow::NoCallbacks)?;
/// let next = commit(&repo, &[main], "Next commit")?;
/// let outdated = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, b"an outdated view");
/// let commands = vec![
///     PushCommand::new(RefName::new("refs/heads/main")?, Some(main), next),
///     // Preparation cannot prove ancestry from an old value it doesn't have, so allow force;
///     // the receiver still checks the expected value.
///     PushCommand::new(RefName::new("refs/heads/release")?, Some(outdated), next)
///         .with_force(ForcePolicy::Allow),
/// ];
///
/// let cancel = AtomicBool::new(false);
/// let objects = repo.objects(PackLimits::default())?;
/// let prepared = PreparedPush::new(&objects, commands, &[main], PushLimits::default(), &cancel)?;
/// let request = LocalPushRequest::new(&prepared);
/// let report = send_local(upstream.git_dir(), request, TransportControl::new(&cancel), |_| {})?;
///
/// assert!(!report.all_succeeded());
/// assert_eq!(report.unpack, Some(Status::Ok));
/// assert_eq!(report.refs[0].status, Some(Status::Ok));
/// assert!(matches!(report.refs[1].status, Some(Status::Rejected(_))));
/// assert_eq!(report.refs[1].rejection_origin, Some(RejectionOrigin::ExpectedValue));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// A successful `send` returns a complete report even when unpacking or every reference failed.
/// An uncertain error retains whatever valid status prefix was received. An `ok` is the server's
/// acknowledgement, not a promise of durability or that another writer has not since moved the ref.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
                    rejection_origin: None,
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
///
/// This classification decides recovery. After [`NotSent`](Self::NotSent), the push attempted no
/// reference updates on the receiver, so retrying it is safe once the cause is fixed. A native
/// local push may still have stored objects, or run receive hooks, in the destination. After
/// [`Uncertain`](Self::Uncertain), some updates may have been applied: use its partial `report`,
/// then read the remote's references before deciding what to send again. Per-reference
/// rejections are not errors; they are reported in [`PushReport`].
///
/// # Examples
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::PackLimits;
/// use girt::push::{
///     LocalPushRequest, PreparedPush, PushError, PushFailure, PushLimits, send_local,
/// };
/// use girt::transport::TransportControl;
/// # let (directory, _, repo, _, _) = include!("../doctest/remote.rs");
///
/// let cancel = AtomicBool::new(false);
/// let objects = repo.objects(PackLimits::default())?;
/// let prepared = PreparedPush::new(&objects, Vec::new(), &[], PushLimits::default(), &cancel)?;
/// let missing = directory.path().join("missing.git");
/// let result = send_local(
///     &missing,
///     LocalPushRequest::new(&prepared),
///     TransportControl::new(&cancel),
///     |_| {},
/// );
/// assert!(matches!(
///     result,
///     Err(PushError::NotSent(PushFailure::Destination(_)))
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PushError {
    /// No update commands were attempted; this operation did not mutate destination refs.
    #[error("push not sent")]
    NotSent(#[source] PushFailure),
    /// Command transmission began. Some or all updates may have happened; do not blindly retry.
    /// Retained acknowledgements are evidence, while missing results require remote inspection.
    #[error("push outcome uncertain")]
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
#[non_exhaustive]
pub enum PushFailure {
    /// Private receive storage could not be removed; inspect the retained operation failure and
    /// report before retrying. Primary publication may already have completed.
    #[error("receive quarantine cleanup failed")]
    QuarantineCleanup {
        /// Private directory whose cleanup was refused or failed; inspect it before removing data.
        path: std::path::PathBuf,
        /// Filesystem cleanup failure.
        #[source]
        source: std::io::Error,
        /// Earlier operation failure, if one occurred before cleanup. This may itself be
        /// [`PushError::Uncertain`] with published refs; inspect its report as well.
        failure: Option<Box<PushError>>,
    },

    /// Native local destination could not be opened.
    #[error("local destination")]
    Destination(#[source] Box<crate::OpenError>),
    /// Native object installation failed before reference publication.
    #[error("local object installation")]
    Install(#[source] Box<crate::fetch::FetchError>),
    /// Native reference publication failed after mutation may have begun.
    #[error("local reference publication")]
    Reference(#[source] Box<crate::refs::ReferenceError>),
    /// Explicit reflog identity is invalid for a new record.
    #[error("invalid push reflog identity")]
    Identity,
    /// A supplied identity uses a different object format from the source repository.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// Sanitized OpenSSH transport or service failure.
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    #[error(transparent)]
    Ssh(crate::transport::ssh::SshError),
    /// Sanitized smart HTTP exchange failure.
    #[cfg(feature = "http")]
    #[error(transparent)]
    Http(crate::transport::http::HttpError),
    /// I/O failure; protocol interruption is returned without retrying.
    #[error("push I/O")]
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
    #[error("push object {id}")]
    Read {
        /// Reachable object being read.
        id: ObjectId,
        /// Concrete storage failure.
        #[source]
        source: crate::ObjectReadError,
    },
    /// Pack construction failed before sending commands.
    #[error("push pack")]
    Pack(#[from] crate::PackWriteError),
    /// Reachable commit payload invalid or unsupported.
    #[error("push commit {id}")]
    Commit {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::CommitError,
    },
    /// Reachable tree payload invalid or unsupported.
    #[error("push tree {id}")]
    Tree {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::TreeError,
    },
    /// Reachable tag payload invalid or unsupported.
    #[error("push tag {id}")]
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
