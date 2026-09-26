//! Conservative, read-only object retention planning for later maintenance.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::refs::{ImportedRecord, RefName, ReflogLimits, Target};
use crate::{Commit, ObjectId, ObjectKind, PackLimits, ReadLimits, Repository, Tag, Tree, index};

#[derive(Debug)]
struct ObservedLog {
    name: RefName,
    position: usize,
    seconds: i128,
    old: ObjectId,
    new: ObjectId,
}

/// Caller policy and aggregate work bounds for one observation.
#[derive(Clone, Debug)]
pub struct RetentionPolicy {
    /// Metadata heads retained by the caller regardless of Git references.
    pub heads: Vec<ObjectId>,
    /// Loose objects with a modification time at or after this instant survive as roots.
    pub recent_cutoff: SystemTime,
    /// Expire reachable reflog records older than this Unix second; `None` retains all.
    pub reflog_expire_before: Option<i128>,
    /// Expire records disconnected from live roots older than this Unix second.
    pub reflog_expire_unreachable_before: Option<i128>,
    /// Maximum names, directory entries, and graph objects visited in each bounded phase.
    pub max_entries: usize,
    /// Maximum graph edges followed in each of the three closure traversals.
    pub max_edges: usize,
    /// Maximum decoded bytes in each closure traversal.
    pub max_bytes: u64,
    /// Bound for each imported history.
    pub reflog: ReflogLimits,
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
            max_reflog_bytes: 64 * 1024 * 1024,
            index: index::Limits::default(),
            packs: PackLimits::default(),
            read: ReadLimits::default(),
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
    /// Distinct root candidates recovered before any failure.
    pub roots: BTreeSet<ObjectId>,
    /// Heads, refs, indexes and recent loose objects before applying reflog policy.
    pub strong_roots: BTreeSet<ObjectId>,
    /// Objects required after applying the caller's reflog expiry cutoffs.
    pub required: BTreeSet<ObjectId>,
    /// One-based record positions eligible for expiry in each stored log.
    pub reflog_expiry_candidates: BTreeMap<RefName, Vec<usize>>,
    observed_logs: Vec<ObservedLog>,
    /// Successfully visited objects reachable from candidates.
    pub reachable: BTreeSet<ObjectId>,
    /// Packs protected by `.keep` or by a recent pack modification time.
    pub protected_packs: BTreeSet<PathBuf>,
    /// Canonical alternate stores used for reads; none is owned for pruning.
    pub alternate_stores: Vec<PathBuf>,
    /// Source snapshot and traversal outcome.
    pub outcome: RetentionOutcome,
}

/// Why a plan can or cannot describe complete reachability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetentionOutcome {
    /// Every discovered root and edge was read under the supplied bounds.
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
    /// and all existing loose objects. The executor must rescan under an exclusion boundary
    /// that covers ref, reflog, index, worktree, loose-object and pack publication before using
    /// this report for mutation.
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
        match walk(self, &store, policy, cancel, &plan.roots) {
            Ok(reachable) => plan.reachable = reachable,
            Err(reason) => {
                plan.fail(reason);
                return plan;
            }
        }
        let live = match walk(self, &store, policy, cancel, &plan.strong_roots) {
            Ok(reachable) => reachable,
            Err(reason) => {
                plan.fail(reason);
                return plan;
            }
        };
        let mut required_roots = plan.strong_roots.clone();
        for record in &plan.observed_logs {
            let live_record = live.contains(&record.old) || live.contains(&record.new);
            let cutoff = if live_record {
                policy.reflog_expire_before
            } else {
                policy.reflog_expire_unreachable_before
            };
            if cutoff.is_some_and(|cutoff| record.seconds < cutoff) {
                plan.reflog_expiry_candidates
                    .entry(record.name.clone())
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
        match walk(self, &store, policy, cancel, &required_roots) {
            Ok(required) => plan.required = required,
            Err(reason) => plan.fail(reason),
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
    let mut directories = vec![repository.common_dir().to_path_buf()];
    let mut reflog_bytes = 0u64;
    let linked = repository
        .worktrees(policy.max_entries, cancel)
        .map_err(|e| e.to_string())?;
    for entry in linked {
        if matches!(
            entry.state,
            crate::WorktreeState::Invalid(_) | crate::WorktreeState::Inaccessible(_)
        ) {
            return Err(format!(
                "invalid worktree registration: {}",
                entry.git_dir.display()
            ));
        }
        directories.push(entry.git_dir);
    }
    for directory in directories {
        check(cancel)?;
        let repo = Repository::open(&directory).map_err(|e| e.to_string())?;
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
        let refs = repo.references().map_err(|e| e.to_string())?;
        let references = refs.list().map_err(|e| e.to_string())?;
        if references.len() > policy.max_entries {
            return Err("reference count limit".into());
        }
        for reference in references {
            let id = match reference.target {
                Target::Direct(id) => Some(id),
                Target::Symbolic(_) => {
                    refs.resolve(&reference.name, 32)
                        .map_err(|e| e.to_string())?
                        .id
                }
            };
            if let Some(id) = id {
                plan.roots.insert(id);
                plan.strong_roots.insert(id);
            }
        }
        if let Some(id) = refs
            .resolve(&RefName::new(b"HEAD").unwrap(), 32)
            .map_err(|e| e.to_string())?
            .id
        {
            plan.roots.insert(id);
            plan.strong_roots.insert(id);
        }
        for name in refs
            .imported_reflog_names(policy.max_entries, cancel)
            .map_err(|e| e.to_string())?
        {
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
                        name: name.clone(),
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
        let object = store
            .read_controlled(id, policy.read, cancel)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("missing reachable object {id}"))?;
        if expected.get(&id).is_some_and(|kind| *kind != object.kind()) {
            return Err(format!("wrong reachable kind {id}"));
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
        reachable.insert(id);
    }
    Ok(reachable)
}

fn check(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("retention scan cancelled".into())
    } else {
        Ok(())
    }
}
