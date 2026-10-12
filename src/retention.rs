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
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::refs::{ImportedRecord, RefName, ReflogLimits, Target};
use crate::{
    Commit, Object, ObjectId, ObjectKind, PackLimits, ReadLimits, Repository, Tag, Tree, index,
};

mod repack;
pub use repack::{RepackError, RepackLimits, RepackPublished};
mod prune;
pub use prune::{MaintenanceIsolation, PruneCause, PruneFailure, PruneReport};
mod expire;
pub use expire::{ExpireCause, ExpireFailure, ExpireReport};
mod retire;
pub use retire::{RetireCause, RetireFailure, RetireReport};
mod compose;
mod inputs;
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

/// Caller policy and aggregate work bounds for one observation.
#[derive(Clone, Debug)]
pub struct RetentionPolicy {
    /// Metadata heads retained by the caller regardless of Git references.
    pub heads: Vec<ObjectId>,
    /// Loose objects modified at or after this instant survive as traversal roots.
    /// Packs modified at or after it remain untouched. Their objects do not classify histories
    /// as live; their dependencies receive a separate protection closure when deletion is
    /// possible.
    pub recent_cutoff: SystemTime,
    /// Expire reachable reflog records older than this Unix second; `None` retains all.
    pub reflog_expire_before: Option<i128>,
    /// Expire records disconnected from live roots older than this Unix second.
    pub reflog_expire_unreachable_before: Option<i128>,
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

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            heads: Vec::new(),
            recent_cutoff: UNIX_EPOCH,
            reflog_expire_before: None,
            reflog_expire_unreachable_before: None,
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
    /// defaults, so set them with struct update syntax.
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
/// classed as live if either endpoint is reachable from non-reflog roots; this conservative rule
/// uses the longer live cutoff when the record straddles live and unreachable history. Callers
/// must not delete an object based solely on `reachable - required` while retained packs, recent
/// objects, alternates, or an independent writer can still refer to it.
#[derive(Debug)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetentionOutcome {
    /// Every root and edge was located; graph-bearing objects were decoded under the bounds.
    Complete,
    /// A source or reachable graph was incomplete. The string names the failed boundary.
    Incomplete(String),
}

impl RetentionPlan {
    /// A complete scan remains advisory until an executor establishes an exclusive writer boundary.
    pub fn is_complete(&self) -> bool {
        self.outcome == RetentionOutcome::Complete
    }

    fn fail(&mut self, reason: impl Into<String>) {
        self.outcome = RetentionOutcome::Incomplete(reason.into());
    }
}

impl Repository {
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
    /// `gc.reflogExpire`, and `gc.reflogExpireUnreachable` configuration. Scans are synchronous
    /// and do not coordinate concurrent writers. The default cutoffs retain all reflog entries
    /// and all existing loose objects. Valid registered private metadata remains a root even when
    /// its checkout is missing or now points to another registration. Malformed or unrelated
    /// registration metadata makes the scan incomplete. Commits, trees and tags are
    /// identity-checked and parsed. Blob edges are located through the same pinned store and
    /// checked for kind, size and bounded delta ancestry; their payload, zlib completion, CRC
    /// and identity are not validated. Blob-shaped untyped roots use ordinary identity-verified
    /// reads so a damaged graph header cannot erase root edges. `max_bytes` counts only decoded
    /// commit, tree and tag bytes, including when a blob root requires a verified read.
    /// Replacement retirement still decodes every retained object from a surviving copy before
    /// deleting an old pack.
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
    /// ```no_run
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::Repository;
    /// use girt::retention::RetentionPolicy;
    /// let repository = Repository::open("/path/to/repository")?;
    /// let plan = repository.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    /// if !plan.is_complete() {
    ///     eprintln!("retention scan incomplete: {:?}", plan.outcome);
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn plan_retention(&self, policy: &RetentionPolicy, cancel: &AtomicBool) -> RetentionPlan {
        let mut plan = RetentionPlan {
            roots: policy.heads.iter().copied().collect(),
            strong_roots: policy.heads.iter().copied().collect(),
            required: BTreeSet::new(),
            reflog_expiry_candidates: BTreeMap::new(),
            observed_logs: Vec::new(),
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
            plan.fail("invalid caller head");
            return plan;
        }
        if policy.heads.len() > policy.max_entries {
            plan.fail("caller head limit");
            return plan;
        }
        if let Err(reason) = collect_roots(self, policy, cancel, &mut plan) {
            plan.fail(reason);
            return plan;
        }
        let store = match self.objects_controlled(
            policy.packs,
            crate::AlternateLimits::default(),
            cancel,
        ) {
            Ok(store) => store,
            Err(error) => {
                plan.fail(error.to_string());
                return plan;
            }
        };
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
            let live_record = live.contains(&record.old) || live.contains(&record.new);
            let cutoff = if live_record {
                policy.reflog_expire_before
            } else {
                policy.reflog_expire_unreachable_before
            };
            if cutoff.is_some_and(|cutoff| record.seconds < cutoff) {
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
                plan.fail(error.to_string());
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
                        plan.fail(error.to_string());
                        return plan;
                    }
                    Ok(_) => {
                        plan.fail("protected pack has no readable index");
                        return plan;
                    }
                }
            }
            // Classification follows normal store precedence, which can select another pack.
            // Authenticate all pinned pack headers before this protection inventory uses them.
            if let Err(error) = store.verify_packs(cancel) {
                plan.fail(error.to_string());
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
                            plan.fail(error.to_string());
                            return plan;
                        }
                    };
                    let (kind, size) = match store.locate(id, policy.read, cancel) {
                        Ok(Some(info)) => info,
                        Ok(None) => {
                            plan.fail("missing protected object");
                            return plan;
                        }
                        Err(error) => {
                            plan.fail(error.to_string());
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
                        plan.fail("protected packed object limit");
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
                plan.fail(error.to_string());
            }
        }
        plan
    }
}

fn collect_roots(
    repository: &Repository,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
    plan: &mut RetentionPlan,
) -> Result<(), String> {
    // Git's hook can designate otherwise old, unreachable objects as recent. Running a hook is
    // outside this read-only library operation, so its presence prevents a complete plan.
    if repository
        .config()
        .value("gc", None, "recentObjectsHook")
        .is_some()
    {
        return Err("gc.recentObjectsHook is unsupported by retention planning".into());
    }
    let mut repositories =
        vec![Repository::open(repository.common_dir()).map_err(|e| e.to_string())?];
    let mut reflog_bytes = 0u64;
    let mut seen_logs = BTreeSet::new();
    let linked = repository
        .worktrees(policy.max_entries, cancel)
        .map_err(|e| e.to_string())?;
    for entry in linked {
        check(cancel)?;
        let retained = repository
            .open_retained_worktree(&entry.git_dir)
            .map_err(|e| e.to_string())?;
        repositories.push(retained);
    }
    for repo in repositories {
        check(cancel)?;
        if repo
            .config()
            .value("extensions", None, "preciousObjects")
            .is_some()
        {
            return Err("precious-object repository forbids deletion planning".into());
        }
        if repo
            .config()
            .value("extensions", None, "partialClone")
            .is_some()
        {
            return Err("partial clone has an incomplete local object inventory".into());
        }
        if repo
            .config()
            .value("gc", None, "recentObjectsHook")
            .is_some()
        {
            return Err("gc.recentObjectsHook is unsupported by retention planning".into());
        }
        let refs = repo
            .references()
            .map_err(|e| e.to_string())?
            .with_reftable_limits(policy.reftable);
        let references = refs
            .list_controlled(policy.max_entries, policy.max_reference_bytes, cancel)
            .map_err(|e| e.to_string())?;
        let mut values: BTreeMap<_, _> = references
            .into_iter()
            .map(|reference| (reference.name, reference.target))
            .collect();
        if let Some(head) = refs
            .head_controlled(policy.max_reference_bytes, cancel)
            .map_err(|e| e.to_string())?
        {
            values.insert(RefName::new(b"HEAD").unwrap(), head);
        }
        for target in values.values() {
            if let Some(id) = resolve_target(target, &values)? {
                plan.roots.insert(id);
                plan.strong_roots.insert(id);
            }
        }
        for name in refs
            .imported_reflog_names(policy.max_entries, cancel)
            .map_err(|e| e.to_string())?
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
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("reflog disappeared during discovery: {name:?}"))?;
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
                    return Err("aggregate reflog byte limit".into());
                }
                if plan.observed_logs.len() >= policy.max_entries {
                    return Err("aggregate reflog record limit".into());
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
                return Err(format!("incomplete reflog {name:?}: {:?}", log.end()));
            }
        }
        collect_pseudorefs(&repo, policy, plan)?;
        if let Some(index) = repo.read_index(policy.index).map_err(|e| e.to_string())? {
            if index.entries().len() > policy.max_entries {
                return Err("index entry limit".into());
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
            return Err("aggregate root limit".into());
        }
    }
    recent_objects(repository, policy, cancel, plan)?;
    Ok(())
}

fn resolve_target(
    target: &Target,
    values: &BTreeMap<RefName, Target>,
) -> Result<Option<ObjectId>, String> {
    let mut target = target;
    let mut visited = BTreeSet::new();
    for _ in 0..=32 {
        match target {
            Target::Direct(id) => return Ok(Some(*id)),
            Target::Symbolic(name) => {
                if !visited.insert(name) {
                    return Err("symbolic reference cycle".into());
                }
                let Some(next) = values.get(name) else {
                    return Ok(None);
                };
                target = next;
            }
        }
    }
    Err("symbolic reference depth limit".into())
}

fn collect_pseudorefs(
    repo: &Repository,
    policy: &RetentionPolicy,
    plan: &mut RetentionPlan,
) -> Result<(), String> {
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
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let mut bytes = Vec::new();
        file.take(policy.reflog.bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > policy.reflog.bytes {
            return Err(format!("pseudoref byte limit: {}", path.display()));
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
                .ok_or_else(|| format!("malformed pseudoref: {}", path.display()))?;
            if line.len() > width && !matches!(line[width], b'\t' | b' ' | b'\r') {
                return Err(format!("malformed pseudoref: {}", path.display()));
            }
            if !id.is_null() {
                plan.roots.insert(id);
                plan.strong_roots.insert(id);
            }
            if plan.roots.len() > policy.max_entries {
                return Err("aggregate root limit".into());
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
) -> Result<(), String> {
    let mut entries = 0;
    for dir in fs::read_dir(repo.object_dir()).map_err(|e| e.to_string())? {
        check(cancel)?;
        entries += 1;
        if entries > policy.max_entries {
            return Err("object directory entry limit".into());
        }
        let dir = dir.map_err(|e| e.to_string())?;
        if !fs::symlink_metadata(dir.path())
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("non-directory object entry".into());
        }
        let name = dir.file_name();
        if name == "pack" {
            for file in fs::read_dir(dir.path()).map_err(|e| e.to_string())? {
                entries += 1;
                if entries > policy.max_entries {
                    return Err("pack entry limit".into());
                }
                let file = file.map_err(|e| e.to_string())?;
                let path = file.path();
                let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                if !metadata.is_file() {
                    return Err(format!("non-file pack entry: {}", path.display()));
                }
                if path.extension().is_some_and(|ext| ext == "keep") {
                    plan.protected_packs.insert(path.with_extension("pack"));
                } else if path.extension().is_some_and(|ext| ext == "pack")
                    && metadata.modified().map_err(|e| e.to_string())? >= policy.recent_cutoff
                {
                    plan.protected_packs.insert(path);
                }
            }
            continue;
        }
        let Some(prefix) = name.to_str() else {
            return Err("non-UTF-8 object fanout".into());
        };
        if prefix == "info" {
            continue;
        }
        if prefix.len() != 2 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("unknown object directory entry".into());
        }
        for file in fs::read_dir(dir.path()).map_err(|e| e.to_string())? {
            check(cancel)?;
            entries += 1;
            if entries > policy.max_entries {
                return Err("loose object entry limit".into());
            }
            let file = file.map_err(|e| e.to_string())?;
            let metadata = fs::symlink_metadata(file.path()).map_err(|e| e.to_string())?;
            if !metadata.is_file() {
                return Err("non-file loose object".into());
            }
            if metadata.modified().map_err(|e| e.to_string())? >= policy.recent_cutoff {
                let Some(suffix) = file.file_name().to_str().map(str::to_owned) else {
                    return Err("non-UTF-8 loose object".into());
                };
                let id = ObjectId::from_hex(repo.object_format(), &format!("{prefix}{suffix}"))
                    .map_err(|e| e.to_string())?;
                plan.roots.insert(id);
                plan.strong_roots.insert(id);
            }
        }
    }
    Ok(())
}

fn walk(
    repo: &Repository,
    store: &crate::Objects,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
    roots: &BTreeSet<ObjectId>,
    object_info: &mut BTreeMap<ObjectId, ObjectInfo>,
) -> Result<BTreeSet<ObjectId>, String> {
    let mut pending: VecDeque<_> = roots.iter().copied().collect();
    let mut reachable = BTreeSet::new();
    let mut expected = BTreeMap::new();
    let mut kinds = BTreeMap::new();
    let mut bytes = 0u64;
    let mut edges = 0usize;
    while let Some(id) = pending.pop_front() {
        check(cancel)?;
        if reachable.contains(&id) {
            continue;
        }
        if reachable.len() >= policy.max_entries {
            return Err("reachable object limit".into());
        }
        let (kind, size) = store
            .locate(id, policy.read, cancel)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("missing reachable object {id}"))?;
        if expected.get(&id).is_some_and(|expected| *expected != kind) {
            return Err(format!("wrong reachable kind {id}"));
        }
        if kind == ObjectKind::Blob {
            // An untyped root has no verified parent's kind assertion. Its ID must bind the
            // blob kind, or a damaged graph header could erase edges and authorize pruning.
            if roots.contains(&id) {
                let object = store
                    .read_controlled(id, policy.read, cancel)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("missing reachable object {id}"))?;
                if object.kind() != kind {
                    return Err(format!("changed root kind {id}"));
                }
            }
            kinds.insert(id, kind);
            object_info.insert(id, ObjectInfo { kind, size });
            reachable.insert(id);
            continue;
        }
        let object = store
            .read_controlled(id, policy.read, cancel)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("missing reachable object {id}"))?;
        if object.kind() != kind {
            return Err(format!("changed reachable kind {id}"));
        }
        bytes = bytes
            .checked_add(object.data().len() as u64)
            .ok_or("reachable byte overflow")?;
        if bytes > policy.max_bytes {
            return Err("reachable byte limit".into());
        }
        let mut add = |target: ObjectId, kind: ObjectKind| -> Result<(), String> {
            edges += 1;
            if edges > policy.max_edges {
                return Err("reachable edge limit".into());
            }
            if target.is_null() {
                return Err(format!("null reachable edge from {id}"));
            }
            if kinds.get(&target).is_some_and(|actual| *actual != kind) {
                return Err(format!("wrong reachable kind {target}"));
            }
            if let Some(previous) = expected.insert(target, kind)
                && previous != kind
            {
                return Err(format!("conflicting reachable kind {target}"));
            }
            pending.push_back(target);
            Ok(())
        };
        match object.kind() {
            ObjectKind::Blob => {}
            ObjectKind::Commit => {
                let commit = Commit::parse(repo.object_format(), object.data())
                    .map_err(|e| e.to_string())?;
                add(commit.tree(), ObjectKind::Tree)?;
                if !repo.shallow_roots().contains(id) {
                    for &parent in commit.parents() {
                        add(parent, ObjectKind::Commit)?;
                    }
                }
            }
            ObjectKind::Tree => {
                let tree =
                    Tree::parse(repo.object_format(), object.data()).map_err(|e| e.to_string())?;
                for entry in tree.entries() {
                    match entry.mode {
                        crate::EntryMode::Gitlink => {}
                        crate::EntryMode::Tree => add(entry.id, ObjectKind::Tree)?,
                        _ => add(entry.id, ObjectKind::Blob)?,
                    }
                }
            }
            ObjectKind::Tag => {
                let tag =
                    Tag::parse(repo.object_format(), object.data()).map_err(|e| e.to_string())?;
                add(tag.target(), tag.target_kind())?;
            }
        }
        kinds.insert(id, object.kind());
        object_info.insert(
            id,
            ObjectInfo {
                kind: object.kind(),
                size: object.data().len() as u64,
            },
        );
        reachable.insert(id);
    }
    Ok(reachable)
}

fn canonical_empty_tree(id: ObjectId) -> Option<Object> {
    // Git can name this fixed tree from a commit or index without storing the object.
    let empty = Object {
        kind: ObjectKind::Tree,
        format: id.format(),
        data: Vec::new(),
    };
    (id == empty.id()).then_some(empty)
}

fn check(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("retention scan cancelled".into())
    } else {
        Ok(())
    }
}
