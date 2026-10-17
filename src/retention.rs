//! Object retention planning, additive repacking, and isolated maintenance.
//!
//! # Retention planning
//!
//! [`Repository::plan_retention`](crate::Repository::plan_retention) observes roots under a
//! [`RetentionPolicy`] and returns a [`RetentionPlan`]. A complete plan explains reachability for
//! that observation; it does not authorize deletion after another writer or reader changes state.
//! [`Repository::repack_retained`](crate::Repository::repack_retained) publishes a new pack without
//! removing old storage.
//!
//! # Maintenance that removes data
//!
//! [`Repository::run_maintenance`](crate::Repository::run_maintenance) composes reflog expiry,
//! pack retirement, and loose-object pruning. Each phase also has a separate method:
//! [`Repository::expire_reflogs`](crate::Repository::expire_reflogs),
//! [`Repository::retire_old_packs`](crate::Repository::retire_old_packs), and
//! [`Repository::prune_unreachable_loose`](crate::Repository::prune_unreachable_loose).
//! The caller supplies [`MaintenanceIsolation`] to exclude external writers and dependent readers
//! for the full operation. Failure reports preserve completed effects; retry from fresh scans under
//! a new isolation guard. Read each method's platform and recovery contract before use.
//!
//! # What keeps an object alive?
//!
//! References are only one source of liveness. The index can stage a blob no commit names yet;
//! reflogs can preserve history after a branch moves; another linked worktree has its own HEAD
//! and index; a consumer may own unpublished heads. Planning includes those roots, object-bearing
//! pseudorefs and recent loose objects, follows verified commit/tree/tag edges, and stops parent
//! traversal at shallow boundaries. Gitlinks refer to another repository and stay external.
//! A partial clone, precious-object repository or unsupported recent-object hook refuses a
//! complete plan rather than treating unknown objects as disposable.
//!
//! Default [`RetentionPolicy`] keeps all reflog records, loose objects and packs regardless of
//! age. Choose a recent cutoff to enable reclamation; choose reflog expiry separately. Loose
//! files at or before the cutoff are eligible only outside the retained closure. Newer loose
//! objects become roots, protecting their dependencies as well. Packs at or after the cutoff
//! remain protected in their entirety. The inclusive loose cutoff matches Git prune expiry;
//! whole-pack protection is deliberately conservative. Girt does not interpret `gc.pruneExpire`
//! here: the application supplies the instant.
//!
//! # Publish, verify, retire
//!
//! Destructive maintenance first establishes caller-owned exclusion, observes one pinned store,
//! publishes and synchronizes a replacement, verifies surviving copies, and only then removes old
//! data. A name in a pack index or a matching checksum cannot establish that an object decodes
//! correctly under the caller's real read limits. Retirement decodes retained objects from the
//! replacement or protected survivor before removing the old copy. Combined delta chains must
//! also fit those limits. Repacking alone is additive and never supplies deletion authorization.
//!
//! `.keep` packs, recent packs and alternate stores are untouchable. Before relying on protected
//! storage to omit a replacement copy, girt verifies the survivor. Dependencies of protected packs
//! receive a separate closure, so they survive without changing reflog liveness classification.
//! Object pruning removes only eligible regular loose files from the primary object directory.
//! After deleting any objects, it also removes derived commit-graph files, which may name pruned
//! commits. Pack retirement removes multi-pack-index artifacts before retiring packs so Git does
//! not consult indexes naming removed storage. Git can rebuild these accelerators; until then,
//! their speedups are unavailable. Pruning does not remove packs, alternates or working files.
//! Standalone prune retains every observed reflog record even if the supplied policy has expiry
//! cutoffs; [`Repository::collect_garbage`](crate::Repository::collect_garbage) expires history
//! before pruning under one guard and advances the exact observation.
//!
//! # Races, damage and recovery
//!
//! Like Git, girt assumes published packs and indexes are immutable. Pinned handles keep one
//! generation through scanning and copying. Metadata/identity checks at mutation boundaries
//! detect observed changes, but cannot prevent a writer racing the final check or changing bytes
//! while restoring metadata. [`MaintenanceIsolation`] must exclude external Git processes,
//! dependent readers and repositories using this store as an alternate for the whole operation.
//! An internal lock alone cannot provide that boundary. Refuse acquisition if it cannot be held.
//!
//! Content reads can rescue a damaged packed entry using another fully decoded and identity-checked
//! copy. That proves only the returned copy; the pack remains known damaged and pinned-input
//! mutation gates refuse it. Cancellation, limits, unsupported types and missing/external delta
//! bases are immediate errors, not fallback triggers. Recovery therefore means restoring or
//! rebuilding verified storage and opening a fresh reader, rather than asking GC to erase the
//! damaged pack. Unique objects in it must remain protected. A plan's blob-header traversal is
//! not a full integrity check, and a no-op prune/expiry report can skip unneeded graph traversal.
//!
//! Failure reports describe completed effects and uncertain publication/unlink boundaries.
//! Cancellation does not roll back earlier phases. Preserve typed causes and source chains,
//! inspect actual storage under exclusion, then retry with fresh scans and a new guard.
//! Composed maintenance, pack retirement and destructive reflog expiry require Unix directory
//! durability support; additive repack has its own less destructive publication contract.
//!
//! # Example
//!
//! Plan retention for a repository whose `main` branch (setup hidden) reaches one commit and its
//! tree, then repack exactly what the plan retains. Planning changes no files, and the additive
//! repack leaves the unreachable loose blob in place:
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//! use std::time::{Duration, SystemTime};
//!
//! use girt::retention::{RepackLimits, RetentionPolicy};
//! # use girt::refs::{Expected, RefName, Target};
//! # use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
//! # let directory = tempfile::tempdir()?;
//! # let path = directory.path().join("repo.git");
//! # let repository = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
//! # let person = Signature {
//! #     name: b"A. Writer".to_vec(),
//! #     email: b"writer@example.com".to_vec(),
//! #     seconds: 1_700_000_000,
//! #     offset_minutes: 0,
//! # };
//! # let loose = repository.loose_objects();
//! # let commit = loose.write_commit(&Commit::new(CommitFields {
//! #     tree: loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?,
//! #     parents: vec![],
//! #     author: person.clone(),
//! #     committer: person.clone(),
//! #     extra_headers: vec![],
//! #     message: b"Initial commit\n".to_vec(),
//! # })?)?;
//! # let main = RefName::new("refs/heads/main")?;
//! # let references = repository.references()?;
//! # references.update_without_reflog(&main, Target::Direct(commit), Expected::Absent)?;
//! let tree = loose.read_commit(commit, 1024)?.tree();
//! let unreachable = loose.write_blob(b"no reference reaches this")?;
//!
//! let policy = // Treat no loose object as recently written, so only references keep objects alive.
//! RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(3600));
//! let cancel = AtomicBool::new(false);
//! let plan = repository.plan_retention(&policy, &cancel);
//! assert!(plan.is_complete(), "{:?}", plan.outcome);
//! assert!(plan.required.contains(&commit) && plan.required.contains(&tree));
//! assert!(!plan.reachable.contains(&unreachable));
//!
//! let published = repository.repack_retained(&policy, RepackLimits::default(), &cancel)?;
//! assert_eq!(published.written.objects, 2);
//! assert!(loose.read_blob(unreachable, 1024).is_ok());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Diagnose a rescued packed object
//!
//! A content read can succeed while the pinned reader remembers packed damage. Verify the store
//! before making maintenance decisions and preserve the diagnostic source chain. This fixture
//! creates damage before opening the reader; modifying published artifacts during a reader's
//! lifetime violates its immutability contract. It deliberately leaves the damaged pack in place.
//!
//! ```rust
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     use std::sync::atomic::AtomicBool;
//!
//!     use girt::{InitKind, ObjectFormat, ObjectKind, PackObject, Repository, write_pack};
//!
//!     let directory = tempfile::tempdir()?;
//!     let repo = Repository::init(
//!         ObjectFormat::Sha1,
//!         directory.path().join("r"),
//!         InitKind::Bare,
//!     )?;
//!     let content = b"content with a verified loose duplicate";
//!     let id = repo.loose_objects().write_blob(content)?;
//!     let mut pack = Vec::new();
//!     let mut index = Vec::new();
//!     let written = write_pack(
//!         ObjectFormat::Sha1,
//!         &[PackObject {
//!             id,
//!             kind: ObjectKind::Blob,
//!             data: content,
//!         }],
//!         &mut pack,
//!         &mut index,
//!         Default::default(),
//!     )?;
//!     // Byte 15 is inside this entry's compressed stream, after its object header.
//!     // This triggers corruption fallback rather than an unsupported header refusal.
//!     // Damage only this private fixture before opening the reader.
//!     pack[15] ^= 0x55;
//!     let packs = repo.git_dir().join("objects/pack");
//!     std::fs::create_dir_all(&packs)?;
//!     let path = packs.join(format!("pack-{}.pack", written.checksum));
//!     std::fs::write(&path, &pack)?;
//!     std::fs::write(path.with_extension("idx"), &index)?;
//!
//!     let objects = repo.objects_with(Default::default())?;
//!     let rescued = objects
//!         .read_with(id, Default::default())?
//!         .expect("loose duplicate");
//!     assert_eq!(rescued.data(), content);
//!     // The damaged bytes remain invalid despite the successful content rescue.
//!     // This fixture also breaks raw checksums; a fresh reader would reject it too.
//!     let diagnostic = objects.verify_packs(&AtomicBool::new(false)).unwrap_err();
//!     let mut cause: &dyn std::error::Error = &diagnostic;
//!     let mut messages = Vec::new();
//!     loop {
//!         messages.push(cause.to_string());
//!         let Some(source) = cause.source() else { break };
//!         cause = source;
//!     }
//!     let diagnostic = messages.join(": ");
//!     assert!(diagnostic.contains(&path.display().to_string()));
//!     assert!(diagnostic.contains("offset 12")); // First entry offset, after the pack header.
//!     eprintln!("{diagnostic}");
//!     // Preserve the original pack for recovery; do not unlink it based on one rescued object.
//!     assert!(path.exists());
//!     Ok(())
//! }
//! ```
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::refs::{ImportedRecord, RefName, ReflogLimits, Target};
use crate::{
    Commit, IdMap, Object, ObjectId, ObjectKind, PackLimits, ReadLimits, Repository, Tag, index,
};

mod error;
pub use error::{RetentionError, RetentionErrorKind};
mod repack;
pub use repack::{RepackError, RepackLimits, RepackPublished};
mod prune;
pub use prune::{MaintenanceIsolation, PruneCause, PruneFailure, PruneReport};
mod config;
pub use config::{ReflogCutoffs, ReflogExpiry, ReflogExpiryError};
mod expire;
pub use expire::{ExpireCause, ExpireFailure, ExpireReport};
mod retire;
pub use retire::{RetireCause, RetireFailure, RetireReport};
mod compose;
mod inputs;
#[cfg(test)]
mod walk_tests;
pub use compose::{MaintenanceCause, MaintenanceFailure, MaintenanceReport};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedLog {
    source: ReflogSource,
    position: usize,
    seconds: i128,
    old: ObjectId,
    new: ObjectId,
}

/// One stored history's name and shared or per-worktree storage root.
///
/// Identical names in different worktrees denote different histories. The root is the canonical
/// Git directory that owns this history, not the `logs` or `reftable` directory itself.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ReflogSource {
    root: PathBuf,
    name: RefName,
}

impl ReflogSource {
    /// Identifies a history under its owning Git directory.
    pub fn new(root: impl Into<PathBuf>, name: RefName) -> Self {
        Self {
            root: root.into(),
            name,
        }
    }

    /// Shared common directory or private worktree Git directory.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Exact byte-preserving history name.
    pub fn name(&self) -> &RefName {
        &self.name
    }
}

/// Explicit reflog expiry mode for one retention observation.
///
/// Keep-all is the safe default. Classified cutoffs preserve girt's existing caller-supplied
/// semantics; configured expiry applies Git's independent general/unreachable rules. Selecting
/// one mode prevents configuration and explicit cutoffs from silently overriding each other.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum ReflogExpiryPolicy {
    /// Retain every history record.
    #[default]
    KeepAll,
    /// Select one cutoff by whether either record endpoint belongs to the live root closure.
    /// This preserves the previous explicit-cutoff API's conservative classification.
    ClassifiedCutoffs {
        /// Expire records with a live endpoint strictly before this Unix second.
        live_before: Option<i128>,
        /// Expire records with neither endpoint live strictly before this Unix second.
        disconnected_before: Option<i128>,
    },
    /// Apply a resolved Git configuration snapshot, including reference patterns and stash policy.
    /// A record qualifies under the general cutoff or, if either endpoint is disconnected, the
    /// unreachable cutoff. Null endpoints count as live in this mode.
    Configured(ReflogExpiry),
}

/// Caller policy and aggregate work bounds for one observation.
///
/// # Retain application-owned heads
///
/// References cannot describe every consumer's live data. Include unpublished heads (for example,
/// an application's operation state) explicitly. Defaults keep every reflog record and all loose
/// objects/packs regardless of age. Enabling reclamation is a separate caller decision.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let cancel = AtomicBool::new(false);
///
/// use girt::retention::RetentionPolicy;
/// let id = repo.loose_objects().write_blob(b"application-owned data")?;
/// let policy = RetentionPolicy::default().with_heads(vec![id]);
/// let plan = repo.plan_retention(&policy, &cancel);
/// assert!(plan.is_complete(), "{:?}", plan.outcome);
/// assert!(plan.required.contains(&id));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RetentionPolicy {
    /// Metadata heads retained by the caller regardless of Git references.
    pub heads: Vec<ObjectId>,
    /// Optional age cutoff for loose roots and protected packs; `None` retains all of them.
    /// With `Some(cutoff)`, loose objects modified strictly after the instant survive as traversal
    /// roots. Loose files at or before it may be pruned only outside the retained closure. Packs
    /// modified at or after it remain untouched. Their objects do not classify histories as live;
    /// their dependencies receive a separate protection closure when deletion is possible.
    /// The default is `None`; use [`Self::with_recent_cutoff`] to enable age-based reclamation.
    pub recent_cutoff: Option<SystemTime>,
    /// Reflog expiry selection; the default retains every record.
    pub reflog_expiry: ReflogExpiryPolicy,
    /// Maximum names, directory entries, and graph objects visited in each bounded phase.
    pub max_entries: usize,
    /// Maximum graph edges followed in each distinct closure traversal.
    pub max_edges: usize,
    /// Maximum decoded commit, tree and tag payload bytes in each closure traversal.
    /// Blob payloads do not count, including ordinary reads of blob-shaped untyped roots.
    /// Blobs reached through verified graph edges are located without decoding.
    pub max_bytes: u64,
    /// Bound for each imported history.
    pub reflog: ReflogLimits,
    /// Bound for each shared or private reftable stack snapshot.
    pub reftable: crate::refs::reftable::StackLimits,
    /// Maximum packed/loose reference bytes per files inventory; HEAD has the same bound.
    pub max_reference_bytes: usize,
    /// Aggregate retained bytes from all discovered histories.
    pub max_reflog_bytes: u64,
    /// Bound for each index decode.
    pub index: index::Limits,
    /// Pack snapshot bounds.
    pub packs: PackLimits,
    /// Per-object read bounds.
    pub read: ReadLimits,
}

impl RetentionPolicy {
    /// Sets [`Self::heads`].
    #[must_use]
    pub fn with_heads(mut self, heads: Vec<ObjectId>) -> Self {
        self.heads = heads;
        self
    }

    /// Enables age-based reclamation by setting [`Self::recent_cutoff`] to `Some(recent_cutoff)`.
    ///
    /// Choose the instant from application policy; girt does not read `gc.pruneExpire` here. At the
    /// instant, loose expiry is inclusive (`mtime <= cutoff`), while pack protection is inclusive
    /// in the other direction (`mtime >= cutoff`). A recent loose root protects its
    /// dependencies too.
    ///
    /// ```
    /// use std::time::{Duration, UNIX_EPOCH};
    ///
    /// use girt::retention::RetentionPolicy;
    /// let cutoff = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    /// let policy = RetentionPolicy::default().with_recent_cutoff(cutoff);
    /// assert_eq!(policy.recent_cutoff, Some(cutoff));
    /// ```
    #[must_use]
    pub fn with_recent_cutoff(mut self, recent_cutoff: SystemTime) -> Self {
        self.recent_cutoff = Some(recent_cutoff);
        self
    }

    /// Sets [`Self::reflog_expiry`].
    #[must_use]
    pub fn with_reflog_expiry(mut self, reflog_expiry: ReflogExpiryPolicy) -> Self {
        self.reflog_expiry = reflog_expiry;
        self
    }

    /// Sets [`Self::max_entries`].
    #[must_use]
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// Sets [`Self::max_edges`].
    #[must_use]
    pub fn with_max_edges(mut self, max_edges: usize) -> Self {
        self.max_edges = max_edges;
        self
    }

    /// Sets [`Self::max_bytes`].
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Sets [`Self::reflog`].
    #[must_use]
    pub fn with_reflog(mut self, reflog: ReflogLimits) -> Self {
        self.reflog = reflog;
        self
    }

    /// Sets [`Self::reftable`].
    #[must_use]
    pub fn with_reftable(mut self, reftable: crate::refs::reftable::StackLimits) -> Self {
        self.reftable = reftable;
        self
    }

    /// Sets [`Self::max_reference_bytes`].
    #[must_use]
    pub fn with_max_reference_bytes(mut self, max_reference_bytes: usize) -> Self {
        self.max_reference_bytes = max_reference_bytes;
        self
    }

    /// Sets [`Self::max_reflog_bytes`].
    #[must_use]
    pub fn with_max_reflog_bytes(mut self, max_reflog_bytes: u64) -> Self {
        self.max_reflog_bytes = max_reflog_bytes;
        self
    }

    /// Sets [`Self::index`].
    #[must_use]
    pub fn with_index(mut self, index: index::Limits) -> Self {
        self.index = index;
        self
    }

    /// Sets [`Self::packs`].
    #[must_use]
    pub fn with_packs(mut self, packs: PackLimits) -> Self {
        self.packs = packs;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            heads: Vec::new(),
            recent_cutoff: None,
            reflog_expiry: ReflogExpiryPolicy::KeepAll,
            max_entries: 1_000_000,
            max_edges: 4_000_000,
            max_bytes: 4 * 1024 * 1024 * 1024,
            reflog: ReflogLimits::default(),
            reftable: crate::refs::reftable::StackLimits::default(),
            max_reference_bytes: 64 * 1024 * 1024,
            max_reflog_bytes: 64 * 1024 * 1024,
            index: index::Limits::default(),
            packs: PackLimits::default(),
            read: ReadLimits::default(),
        }
    }
}

impl RetentionPolicy {
    /// Returns the default policy with limits for a repository the caller trusts.
    ///
    /// Git bounds none of these values; see [`ReadLimits::trusted`]. Cutoffs and heads keep their
    /// defaults; select them with the policy's builder methods.
    pub fn trusted() -> Self {
        Self {
            max_entries: usize::MAX / 4,
            max_edges: usize::MAX / 4,
            max_bytes: u64::MAX / 4,
            reflog: ReflogLimits::trusted(),
            reftable: crate::refs::reftable::StackLimits::trusted(),
            max_reference_bytes: usize::MAX / 4,
            max_reflog_bytes: u64::MAX / 4,
            index: index::Limits::trusted(),
            packs: PackLimits::trusted(),
            read: ReadLimits::trusted(),
            ..Self::default()
        }
    }
}

/// A read-only observation. Only `Complete` establishes a closed reachable set for its inputs.
/// Even complete reports never authorize deletion: the executor must exclude writers, rediscover
/// roots/recent objects and compare storage generations before it acts. `reachable` includes every
/// imported reflog candidate, while `required` applies the supplied expiry cutoffs. A record is
/// classed as live under [`ReflogExpiryPolicy::ClassifiedCutoffs`] if either endpoint is reachable
/// from non-reflog roots; this conservative rule uses the live cutoff for a straddling record.
/// [`ReflogExpiryPolicy::Configured`] instead applies Git's independent expiry rules. Callers
/// must not delete an object based solely on `reachable - required` while retained packs, recent
/// objects, alternates, or an independent writer can still refer to it. The plan retains the pinned
/// object store used for its observations until dropped; execution reuses those exact handles.
/// Drop the plan promptly after use: its handles keep retired packs' disk space allocated. On
/// Windows a deleted pinned file remains pending deletion and can prevent recreating its name.
///
/// # Inspect completion before using sets
///
/// Partial sets explain observed progress; they cannot establish that another object is garbage.
/// The executor constructs its own fresh plan under isolation. A complete plan also does not
/// certify every blob payload: graph traversal locates typed blob edges without decoding them.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let cancel = AtomicBool::new(false);
///
/// use girt::retention::{RetentionOutcome, RetentionPolicy};
/// let id = repo
///     .loose_objects()
///     .write_blob(b"unreferenced, but retained by default")?;
/// let plan = repo.plan_retention(&RetentionPolicy::default(), &cancel);
/// match plan.outcome {
///     RetentionOutcome::Complete => assert!(plan.required.contains(&id)),
///     RetentionOutcome::Incomplete(error) => return Err(error.into()),
///     _ => return Err("unknown retention outcome".into()),
/// }
/// drop(plan); // Release pinned pack handles promptly.
///
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct RetentionPlan {
    /// Distinct explicit root candidates recovered before any failure.
    pub roots: BTreeSet<ObjectId>,
    /// Heads, refs, indexes and recent loose objects before applying reflog policy.
    pub strong_roots: BTreeSet<ObjectId>,
    /// Objects required after applying the caller's reflog expiry cutoffs.
    pub required: BTreeSet<ObjectId>,
    /// One-based record positions eligible for expiry in each stored log.
    pub reflog_expiry_candidates: BTreeMap<ReflogSource, Vec<usize>>,
    observed_logs: Vec<ObservedLog>,
    // Execution reuses the exact handles and freshness stamps that supplied graph evidence.
    store: Option<crate::Objects>,
    // Kinds and logical sizes from decoded graph objects and located blob headers.
    object_info: BTreeMap<ObjectId, ObjectInfo>,
    // Separate closure protects dependencies of surviving packs without affecting reflog liveness.
    protected_objects: BTreeSet<ObjectId>,
    /// Successfully visited objects reachable from candidates.
    pub reachable: BTreeSet<ObjectId>,
    /// Packs protected by `.keep` or by a recent pack modification time.
    pub protected_packs: BTreeSet<PathBuf>,
    /// Canonical alternate stores used for reads; none is owned for pruning.
    pub alternate_stores: Vec<PathBuf>,
    /// Source snapshot and traversal outcome.
    pub outcome: RetentionOutcome,
}

#[derive(Debug)]
struct ObjectInfo {
    kind: ObjectKind,
    size: u64,
}

/// Why a plan can or cannot describe complete reachability.
///
/// Cancellation from a lower-level inventory can be wrapped in [`RetentionError::Read`]. Keep
/// its typed source: the recovery category alone does not distinguish every storage condition.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::retention::{RetentionError, RetentionErrorKind, RetentionOutcome, RetentionPolicy};
/// use girt::{InitKind, ObjectFormat, Repository, WorktreeError};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(true));
/// let RetentionOutcome::Incomplete(error) = &plan.outcome else {
///     panic!("cancelled inventory cannot establish completeness");
/// };
/// assert_eq!(error.kind(), RetentionErrorKind::Read);
/// let RetentionError::Read(source) = error else {
///     panic!("expected inventory failure");
/// };
/// assert!(matches!(
///     source.downcast_ref::<WorktreeError>(),
///     Some(WorktreeError::Cancelled)
/// ));
/// assert!(!plan.is_complete());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum RetentionOutcome {
    /// Every root and edge was located; graph-bearing objects were decoded under the bounds.
    Complete,
    /// A source or reachable graph was incomplete. The cause identifies the failed boundary.
    Incomplete(RetentionError),
}

impl RetentionPlan {
    /// A complete scan remains advisory until an executor establishes an exclusive writer boundary.
    pub fn is_complete(&self) -> bool {
        matches!(self.outcome, RetentionOutcome::Complete)
    }

    fn check_packs_unchanged(&self, cancel: &AtomicBool) -> Result<(), crate::ObjectReadError> {
        if let Some(store) = &self.store {
            store.check_packs_unchanged(cancel)?;
        }
        Ok(())
    }

    fn fail(&mut self, reason: RetentionError) {
        self.outcome = RetentionOutcome::Incomplete(reason);
    }
}

impl Repository {
    // This observation never verifies object history and cannot authorize a mutation by itself.
    fn observe_retention_roots(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> RetentionPlan {
        let mut plan = RetentionPlan {
            roots: policy.heads.iter().copied().collect(),
            strong_roots: policy.heads.iter().copied().collect(),
            required: BTreeSet::new(),
            reflog_expiry_candidates: BTreeMap::new(),
            observed_logs: Vec::new(),
            store: None,
            object_info: BTreeMap::new(),
            protected_objects: BTreeSet::new(),
            reachable: BTreeSet::new(),
            protected_packs: BTreeSet::new(),
            alternate_stores: Vec::new(),
            outcome: RetentionOutcome::Complete,
        };
        if policy
            .heads
            .iter()
            .any(|id| id.format() != self.object_format() || id.is_null())
        {
            plan.fail(RetentionError::InvalidCallerHead);
            return plan;
        }
        if policy.heads.len() > policy.max_entries {
            plan.fail(RetentionError::CallerHeadLimit);
            return plan;
        }
        if let Err(reason) = collect_roots(self, policy, cancel, &mut plan) {
            plan.fail(reason);
            return plan;
        }
        plan
    }

    /// Collects conservative retention roots and their reachable object closure.
    ///
    /// Includes caller heads, all live refs, independently discovered histories (including old
    /// and new IDs regardless of expiry), object-bearing pseudorefs, main and linked HEAD/index
    /// data, and recent loose objects. Gitlinks remain external. Shallow commits stop parent
    /// traversal at the declared boundary. Alternate storage supplies reads but is never owned by
    /// this plan. All packs are outside deletion scope; protected pack names are reported for
    /// later repacking policy. Unknown or incomplete reads return an incomplete report, preserving
    /// recovered candidates. `gc.recentObjectsHook`, precious objects and partial clones block
    /// completion. The caller supplies cutoffs in place of Git's `gc.pruneExpire`,
    /// reflog configuration, or selects [`ReflogExpiryPolicy::Configured`] with a resolved
    /// snapshot. Scans are synchronous and do not coordinate concurrent writers. The default
    /// policy retains all reflog entries, loose objects and packs regardless of their timestamps.
    /// Valid registered private metadata remains a root even when its checkout is missing or
    /// now points to another registration. Malformed or unrelated registration metadata makes
    /// the scan incomplete. Commits, trees and tags are identity-checked and parsed. Blob edges
    /// are located through the same pinned store and checked for kind, size and bounded delta
    /// ancestry; their payload, zlib completion, CRC and identity are not validated.
    /// Blob-shaped untyped roots use ordinary identity-verified reads so a damaged graph header
    /// cannot erase root edges. `max_bytes` counts only decoded commit, tree and tag bytes,
    /// including when a blob root requires a verified read. Replacement retirement still
    /// decodes every retained object from a surviving copy before deleting an old pack.
    ///
    /// Recent packs and `.keep` packs remain protected without contributing to `roots`,
    /// `strong_roots`, `reachable` or reflog live classification. If loose or unprotected packed
    /// storage might be deleted, a separate closure preserves their dependencies. Unknown or
    /// missing dependencies make the plan incomplete. Raw pinned-pack checksums and CRCs
    /// authenticate headers before the protection inventory selects graph-bearing traversal
    /// roots; protected blob IDs remain outside traversal roots. Equal root sets reuse their
    /// closure. Blob-copy header inventories reject a located blob that hides a graph-bearing copy
    /// at the same ID. Their names and entries are bounded by `max_entries`; blob edge payloads
    /// stay uninflated. Limits apply to each distinct traversal once per plan. Destructive
    /// composite operations use one plan under isolation and revalidate input fingerprints
    /// before mutation.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::Repository;
    /// use girt::retention::RetentionPolicy;
    /// let directory = tempfile::tempdir()?;
    /// let repository = Repository::init(
    ///     girt::ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     girt::InitKind::Bare,
    /// )?;
    /// let plan = repository.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    /// if !plan.is_complete() {
    ///     eprintln!("retention scan incomplete: {:?}", plan.outcome);
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn plan_retention(&self, policy: &RetentionPolicy, cancel: &AtomicBool) -> RetentionPlan {
        let _headers = crate::pack::HeaderScope::new();
        let mut plan = self.observe_retention_roots(policy, cancel);
        if !plan.is_complete() {
            return plan;
        }
        let store = match self.objects_controlled(
            policy.packs,
            crate::AlternateLimits::default(),
            cancel,
        ) {
            Ok(store) => store,
            Err(error) => {
                plan.fail(RetentionError::read(error));
                return plan;
            }
        };
        // Opening checks index structure only. A plan authorizes deletion, so it also requires
        // every pinned index's checksum before trusting the identities that index claims.
        if let Err(error) = store.verify_indexes(cancel) {
            plan.fail(RetentionError::read(error));
            return plan;
        }
        plan.alternate_stores = store
            .store_directories()
            .skip(1)
            .map(PathBuf::from)
            .collect();
        let roots = plan.roots.clone();
        let strong_roots = plan.strong_roots.clone();
        match walk(self, &store, policy, cancel, &roots, &mut plan.object_info) {
            Ok(reachable) => plan.reachable = reachable,
            Err(reason) => {
                plan.fail(reason);
                return plan;
            }
        }
        // Equal root sets share the same located and validated closure.
        let live = if strong_roots == roots {
            plan.reachable.clone()
        } else {
            match walk(
                self,
                &store,
                policy,
                cancel,
                &strong_roots,
                &mut plan.object_info,
            ) {
                Ok(reachable) => reachable,
                Err(reason) => {
                    plan.fail(reason);
                    return plan;
                }
            }
        };
        let mut required_roots = strong_roots.clone();
        for record in &plan.observed_logs {
            let expires = match &policy.reflog_expiry {
                ReflogExpiryPolicy::KeepAll => false,
                ReflogExpiryPolicy::ClassifiedCutoffs {
                    live_before,
                    disconnected_before,
                } => {
                    let live_record = live.contains(&record.old) || live.contains(&record.new);
                    let cutoff = if live_record {
                        live_before
                    } else {
                        disconnected_before
                    };
                    cutoff.is_some_and(|cutoff| record.seconds < cutoff)
                }
                ReflogExpiryPolicy::Configured(expiry) => {
                    let cutoffs = match expiry.cutoffs(record.source.name()) {
                        Ok(cutoffs) => cutoffs,
                        Err(error) => {
                            plan.fail(RetentionError::read(error));
                            return plan;
                        }
                    };
                    let live_record = (record.old.is_null() || live.contains(&record.old))
                        && (record.new.is_null() || live.contains(&record.new));
                    cutoffs
                        .general
                        .is_some_and(|cutoff| record.seconds < cutoff)
                        || (!live_record
                            && cutoffs
                                .unreachable
                                .is_some_and(|cutoff| record.seconds < cutoff))
                }
            };
            if expires {
                plan.reflog_expiry_candidates
                    .entry(record.source.clone())
                    .or_default()
                    .push(record.position);
            } else {
                required_roots.extend(
                    [record.old, record.new]
                        .into_iter()
                        .filter(|id| !id.is_null()),
                );
            }
        }
        if required_roots == roots {
            plan.required = plan.reachable.clone();
        } else if required_roots == strong_roots {
            plan.required = live;
        } else {
            match walk(
                self,
                &store,
                policy,
                cancel,
                &required_roots,
                &mut plan.object_info,
            ) {
                Ok(required) => plan.required = required,
                Err(reason) => plan.fail(reason),
            }
        }
        // Surviving packs can refer to objects held only in deletable storage. Their closure
        // protects those dependencies separately from the roots that classify reflog liveness.
        let has_candidates = match prune::candidates(self, policy, cancel) {
            Ok(candidates) => !candidates.is_empty(),
            Err(error) => {
                plan.fail(RetentionError::from_prune(error));
                return plan;
            }
        };
        let primary_packs = store.primary_packs_with_paths();
        let has_old_pack = primary_packs
            .into_iter()
            .any(|(path, _)| !plan.protected_packs.contains(path));
        if (has_candidates || has_old_pack) && !plan.protected_packs.is_empty() {
            let indexed: BTreeSet<_> = store
                .primary_packs_with_paths()
                .into_iter()
                .map(|(path, _)| path.to_path_buf())
                .collect();
            for path in &plan.protected_packs {
                if indexed.contains(path) {
                    continue;
                }
                match fs::symlink_metadata(path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        plan.fail(RetentionError::read(error));
                        return plan;
                    }
                    Ok(_) => {
                        plan.fail(RetentionError::MissingProtectedIndex);
                        return plan;
                    }
                }
            }
            // Classification follows normal store precedence, which can select another pack.
            // Authenticate all pinned pack headers before this protection inventory uses them.
            if let Err(error) = store.verify_packs(cancel) {
                plan.fail(RetentionError::read(error));
                return plan;
            }
            let mut protected_roots = BTreeSet::new();
            for (path, pack) in store.primary_packs_with_paths() {
                if !plan.protected_packs.contains(path) {
                    continue;
                }
                for id in pack.identities() {
                    let id = match id {
                        Ok(id) => id,
                        Err(error) => {
                            plan.fail(RetentionError::read(error));
                            return plan;
                        }
                    };
                    let (kind, size) = match store.locate(id, policy.read, cancel) {
                        Ok(Some(info)) => info,
                        Ok(None) => {
                            plan.fail(RetentionError::MissingProtectedObject);
                            return plan;
                        }
                        Err(error) => {
                            plan.fail(RetentionError::read(error));
                            return plan;
                        }
                    };
                    plan.protected_objects.insert(id);
                    if kind == ObjectKind::Blob {
                        plan.object_info
                            .entry(id)
                            .or_insert(ObjectInfo { kind, size });
                    } else {
                        protected_roots.insert(id);
                    }
                    if plan.protected_objects.len() > policy.max_entries {
                        plan.fail(RetentionError::ProtectedPackedObjectLimit);
                        return plan;
                    }
                }
            }
            match walk(
                self,
                &store,
                policy,
                cancel,
                &protected_roots,
                &mut plan.object_info,
            ) {
                Ok(objects) => plan.protected_objects.extend(objects),
                Err(reason) => plan.fail(reason),
            }
        }
        if plan.is_complete() {
            let is_blob = |id| {
                plan.object_info
                    .get(&id)
                    .is_some_and(|info| info.kind == ObjectKind::Blob)
            };
            if let Err(error) =
                store.check_retention_blob_kinds(policy.read, policy.max_entries, cancel, is_blob)
            {
                plan.fail(RetentionError::read(error));
            }
        }
        if let Err(error) = store.check_packs_unchanged(cancel) {
            plan.fail(RetentionError::read(error));
        }
        plan.store = Some(store);
        plan
    }
}

fn collect_roots(
    repository: &Repository,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
    plan: &mut RetentionPlan,
) -> Result<(), RetentionError> {
    // Git's hook can designate otherwise old, unreachable objects as recent. Running a hook is
    // outside this read-only library operation, so its presence prevents a complete plan.
    if repository
        .config()
        .value("gc", None, "recentObjectsHook")
        .is_some()
    {
        return Err(RetentionError::RecentObjectsHook);
    }
    let mut repositories =
        vec![Repository::open(repository.common_dir()).map_err(RetentionError::read)?];
    let mut reflog_bytes = 0u64;
    let mut seen_logs = BTreeSet::new();
    let linked = repository
        .worktrees(policy.max_entries, cancel)
        .map_err(RetentionError::read)?;
    for entry in linked {
        check(cancel)?;
        let retained = repository
            .open_retained_worktree(&entry.git_dir)
            .map_err(RetentionError::read)?;
        repositories.push(retained);
    }
    for repo in repositories {
        check(cancel)?;
        if repo
            .config()
            .value("extensions", None, "preciousObjects")
            .is_some()
        {
            return Err(RetentionError::PreciousObjects);
        }
        if repo
            .config()
            .value("extensions", None, "partialClone")
            .is_some()
        {
            return Err(RetentionError::PartialClone);
        }
        if repo
            .config()
            .value("gc", None, "recentObjectsHook")
            .is_some()
        {
            return Err(RetentionError::RecentObjectsHook);
        }
        let refs = repo
            .references()
            .map_err(RetentionError::read)?
            .with_reftable_limits(policy.reftable);
        let references = refs
            .list_controlled(policy.max_entries, policy.max_reference_bytes, cancel)
            .map_err(RetentionError::read)?;
        let mut values: BTreeMap<_, _> = references
            .into_iter()
            .map(|reference| (reference.name, reference.target))
            .collect();
        if let Some(head) = refs
            .head_controlled(policy.max_reference_bytes, cancel)
            .map_err(RetentionError::read)?
        {
            values.insert(RefName::HEAD, head);
        }
        for target in values.values() {
            if let Some(id) = resolve_target(target, &values)? {
                plan.roots.insert(id);
                plan.strong_roots.insert(id);
            }
        }
        for name in refs
            .imported_reflog_names(policy.max_entries, cancel)
            .map_err(RetentionError::read)?
        {
            let root = if name.per_worktree() {
                repo.git_dir()
            } else {
                repo.common_dir()
            };
            let source = ReflogSource::new(root, name.clone());
            if !seen_logs.insert(source.clone()) {
                continue;
            }
            let log = refs
                .imported_reflog(&name, policy.reflog, cancel)
                .map_err(RetentionError::read)?
                .ok_or_else(|| RetentionError::ReflogDisappeared(name.clone()))?;
            plan.roots.extend(log.recoverable_roots());
            for (position, record) in log.records().iter().enumerate() {
                let size = match record {
                    ImportedRecord::File { bytes, .. } => bytes.len(),
                    ImportedRecord::Reftable { value, .. } => value
                        .name
                        .len()
                        .saturating_add(value.email.len())
                        .saturating_add(value.message.len())
                        .saturating_add(2 * repo.object_format().digest_len() + 18),
                };
                reflog_bytes = reflog_bytes.saturating_add(size as u64);
                if reflog_bytes > policy.max_reflog_bytes {
                    return Err(RetentionError::AggregateReflogByteLimit);
                }
                if plan.observed_logs.len() >= policy.max_entries {
                    return Err(RetentionError::AggregateReflogRecordLimit);
                }
                if let Ok(fields) = record.fields() {
                    plan.observed_logs.push(ObservedLog {
                        source: source.clone(),
                        position: position + 1,
                        seconds: fields.seconds,
                        old: fields.old,
                        new: fields.new,
                    });
                }
            }
            if !log.is_complete() {
                return Err(RetentionError::reflog(log));
            }
        }
        collect_pseudorefs(&repo, policy, plan)?;
        if let Some(index) = repo
            .read_index(policy.index)
            .map_err(RetentionError::read)?
        {
            if index.entries().len() > policy.max_entries {
                return Err(RetentionError::IndexEntryLimit);
            }
            for entry in index.entries() {
                if !entry.intent_to_add && entry.mode != index::Mode::Gitlink && !entry.id.is_null()
                {
                    plan.roots.insert(entry.id);
                    plan.strong_roots.insert(entry.id);
                }
            }
        }
        if plan.roots.len() > policy.max_entries {
            return Err(RetentionError::AggregateRootLimit);
        }
    }
    recent_objects(repository, policy, cancel, plan)?;
    Ok(())
}

fn resolve_target(
    target: &Target,
    values: &BTreeMap<RefName, Target>,
) -> Result<Option<ObjectId>, RetentionError> {
    let mut target = target;
    let mut visited = BTreeSet::new();
    for _ in 0..=32 {
        match target {
            Target::Direct(id) => return Ok(Some(*id)),
            Target::Symbolic(name) => {
                if !visited.insert(name) {
                    return Err(RetentionError::SymbolicReferenceCycle);
                }
                let Some(next) = values.get(name) else {
                    return Ok(None);
                };
                target = next;
            }
        }
    }
    Err(RetentionError::SymbolicReferenceDepthLimit)
}

fn collect_pseudorefs(
    repo: &Repository,
    policy: &RetentionPolicy,
    plan: &mut RetentionPlan,
) -> Result<(), RetentionError> {
    // These object-bearing pseudorefs do not appear in refs.list(). Treat unfamiliar framing
    // as incomplete rather than dropping a possible retention root.
    for name in [
        "ORIG_HEAD",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_HEAD",
        "REBASE_HEAD",
        "AUTO_MERGE",
        "FETCH_HEAD",
    ] {
        let path = repo.git_dir().join(name);
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(RetentionError::PseudorefIo {
                    path: path.clone(),
                    source: error,
                });
            }
        };
        let mut bytes = Vec::new();
        file.take(policy.reflog.bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(RetentionError::read)?;
        if bytes.len() > policy.reflog.bytes {
            return Err(RetentionError::PseudorefByteLimit(path.clone()));
        }
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let width = repo.object_format().digest_len() * 2;
            let id = line
                .get(..width)
                .and_then(|value| std::str::from_utf8(value).ok())
                .and_then(|value| ObjectId::from_hex(repo.object_format(), value).ok())
                .ok_or_else(|| RetentionError::MalformedPseudoref(path.clone()))?;
            if line.len() > width && !matches!(line[width], b'\t' | b' ' | b'\r') {
                return Err(RetentionError::MalformedPseudoref(path.clone()));
            }
            if !id.is_null() {
                plan.roots.insert(id);
                plan.strong_roots.insert(id);
            }
            if plan.roots.len() > policy.max_entries {
                return Err(RetentionError::AggregateRootLimit);
            }
        }
    }
    Ok(())
}

fn recent_objects(
    repo: &Repository,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
    plan: &mut RetentionPlan,
) -> Result<(), RetentionError> {
    let mut entries = 0;
    for dir in fs::read_dir(repo.object_dir()).map_err(RetentionError::read)? {
        check(cancel)?;
        entries += 1;
        if entries > policy.max_entries {
            return Err(RetentionError::ObjectDirectoryEntryLimit);
        }
        let dir = dir.map_err(RetentionError::read)?;
        if !fs::symlink_metadata(dir.path())
            .map_err(RetentionError::read)?
            .is_dir()
        {
            return Err(RetentionError::NonDirectoryObjectEntry);
        }
        let name = dir.file_name();
        if name == "pack" {
            for file in fs::read_dir(dir.path()).map_err(RetentionError::read)? {
                entries += 1;
                if entries > policy.max_entries {
                    return Err(RetentionError::PackEntryLimit);
                }
                let file = file.map_err(RetentionError::read)?;
                let path = file.path();
                let metadata = fs::symlink_metadata(&path).map_err(RetentionError::read)?;
                if !metadata.is_file() {
                    return Err(RetentionError::NonFilePackEntry(path.clone()));
                }
                if path.extension().is_some_and(|ext| ext == "keep") {
                    plan.protected_packs.insert(path.with_extension("pack"));
                } else if path.extension().is_some_and(|ext| ext == "pack") {
                    let modified = metadata.modified().map_err(RetentionError::read)?;
                    if policy.recent_cutoff.is_none_or(|cutoff| modified >= cutoff) {
                        plan.protected_packs.insert(path);
                    }
                }
            }
            continue;
        }
        let Some(prefix) = name.to_str() else {
            return Err(RetentionError::NonUtf8ObjectFanout);
        };
        if prefix == "info" {
            continue;
        }
        if prefix.len() != 2 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RetentionError::UnknownObjectDirectoryEntry);
        }
        for file in fs::read_dir(dir.path()).map_err(RetentionError::read)? {
            check(cancel)?;
            entries += 1;
            if entries > policy.max_entries {
                return Err(RetentionError::LooseObjectEntryLimit);
            }
            let file = file.map_err(RetentionError::read)?;
            let metadata = fs::symlink_metadata(file.path()).map_err(RetentionError::read)?;
            if !metadata.is_file() {
                return Err(RetentionError::NonFileLooseObject);
            }
            let modified = metadata.modified().map_err(RetentionError::read)?;
            if policy.recent_cutoff.is_none_or(|cutoff| modified > cutoff) {
                let Some(suffix) = file.file_name().to_str().map(str::to_owned) else {
                    return Err(RetentionError::NonUtf8LooseObject);
                };
                let id = ObjectId::from_hex(repo.object_format(), &format!("{prefix}{suffix}"))
                    .map_err(RetentionError::malformed)?;
                plan.roots.insert(id);
                plan.strong_roots.insert(id);
            }
        }
    }
    Ok(())
}

/// What the walk knows about one identity it has queued.
struct Node {
    /// Kind a verified parent's edge asserts; `None` for a root no edge has named yet.
    expected: Option<ObjectKind>,
    /// Kind of the copy the walk read; `None` until it is read.
    actual: Option<ObjectKind>,
}

fn walk(
    repo: &Repository,
    store: &crate::Objects,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
    roots: &BTreeSet<ObjectId>,
    object_info: &mut BTreeMap<ObjectId, ObjectInfo>,
) -> Result<BTreeSet<ObjectId>, RetentionError> {
    // Each identity is queued once, when it is first named, and keeps one entry for its asserted
    // and read kinds. Trees name most identities many times over (tens of millions of edges on a
    // large history), so an edge to a queued identity costs one hashed probe and no queue entry.
    let mut nodes = IdMap::default();
    let mut pending = VecDeque::with_capacity(roots.len());
    for &root in roots {
        nodes.insert(
            root,
            Node {
                expected: None,
                actual: None,
            },
        );
        pending.push_back(root);
    }
    let mut visited = 0usize;
    let mut bytes = 0u64;
    let mut edges = 0usize;
    let mut blobs = Vec::new();
    while let Some(id) = pending.pop_front() {
        check(cancel)?;
        if visited >= policy.max_entries {
            return Err(RetentionError::ReachableObjectLimit);
        }
        visited += 1;
        let expected = nodes.get(&id).and_then(|node| node.expected);
        // Only blobs are located, after the graph walk and in pack order: they add no edges, and
        // locating them in graph order would read headers scattered across the pack. An edge
        // to a commit, tree or tag is decoded for its own edges, so it is never located first.
        if expected == Some(ObjectKind::Blob) && !roots.contains(&id) {
            blobs.push(id);
            continue;
        }
        // An untyped root has no verified parent's kind assertion, so its identity-checked read
        // binds its kind; a damaged graph header could otherwise erase edges and allow pruning.
        let object = store
            .read_controlled(id, policy.read, cancel)
            .map_err(RetentionError::read)?
            .ok_or_else(|| RetentionError::MissingObject(id))?;
        let kind = object.kind();
        // An edge named this identity with another kind after it was queued.
        let expected = nodes.get(&id).and_then(|node| node.expected);
        if expected.is_some_and(|asserted| asserted != kind) {
            return Err(RetentionError::WrongObjectKind(id));
        }
        if let Some(node) = nodes.get_mut(&id) {
            node.actual = Some(kind);
        }
        object_info.insert(
            id,
            ObjectInfo {
                kind,
                size: object.data().len() as u64,
            },
        );
        if kind == ObjectKind::Blob {
            continue;
        }
        bytes = bytes
            .checked_add(object.data().len() as u64)
            .ok_or(RetentionError::ReachableByteOverflow)?;
        if bytes > policy.max_bytes {
            return Err(RetentionError::ReachableByteLimit);
        }
        let mut add = |target: ObjectId, kind: ObjectKind| -> Result<(), RetentionError> {
            edges += 1;
            if edges > policy.max_edges {
                return Err(RetentionError::ReachableEdgeLimit);
            }
            if target.is_null() {
                return Err(RetentionError::NullEdge(id));
            }
            match nodes.entry(target) {
                Entry::Occupied(mut entry) => {
                    let node = entry.get_mut();
                    if node.actual.is_some_and(|actual| actual != kind) {
                        return Err(RetentionError::WrongObjectKind(target));
                    }
                    match node.expected {
                        Some(previous) if previous != kind => {
                            return Err(RetentionError::ConflictingObjectKind(target));
                        }
                        Some(_) => {}
                        None => node.expected = Some(kind),
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(Node {
                        expected: Some(kind),
                        actual: None,
                    });
                    pending.push_back(target);
                }
            }
            Ok(())
        };
        match kind {
            ObjectKind::Blob => {}
            ObjectKind::Commit => {
                let commit = Commit::parse(repo.object_format(), object.data())
                    .map_err(RetentionError::malformed)?;
                add(commit.tree(), ObjectKind::Tree)?;
                if !repo.shallow_roots().contains(id) {
                    for &parent in commit.parents() {
                        add(parent, ObjectKind::Commit)?;
                    }
                }
            }
            ObjectKind::Tree => {
                // Borrowed entries: a tree is parsed only for its edges, so copying every name
                // would allocate once per entry.
                for entry in crate::tree::RawEntries::new(repo.object_format(), object.data()) {
                    let (mode, _, target) = entry.map_err(RetentionError::malformed)?;
                    match mode {
                        crate::EntryMode::Gitlink => {}
                        crate::EntryMode::Tree => add(target, ObjectKind::Tree)?,
                        _ => add(target, ObjectKind::Blob)?,
                    }
                }
            }
            ObjectKind::Tag => {
                let tag = Tag::parse(repo.object_format(), object.data())
                    .map_err(RetentionError::malformed)?;
                add(tag.target(), tag.target_kind())?;
            }
        }
    }
    let located = store
        .locate_all(blobs, policy.read, cancel)
        .map_err(RetentionError::read)?;
    for (id, located) in located {
        let (kind, size) = located.ok_or_else(|| RetentionError::MissingObject(id))?;
        if kind != ObjectKind::Blob {
            return Err(RetentionError::WrongObjectKind(id));
        }
        object_info.insert(id, ObjectInfo { kind, size });
    }
    Ok(nodes.into_keys().collect())
}

fn canonical_empty_tree(id: ObjectId) -> Option<Object> {
    // Git can name this fixed tree from a commit or index without storing the object.
    let empty = Object {
        kind: ObjectKind::Tree,
        format: id.format(),
        data: Default::default(),
    };
    (id == empty.id()).then_some(empty)
}

fn check(cancel: &AtomicBool) -> Result<(), RetentionError> {
    if cancel.load(Ordering::Relaxed) {
        Err(RetentionError::Cancelled)
    } else {
        Ok(())
    }
}
